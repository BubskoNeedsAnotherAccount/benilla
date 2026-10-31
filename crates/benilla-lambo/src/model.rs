//! The car glb, split into a body and its wheels so the wheels can roll and steer. The glb is one
//! mesh, so each connected piece of it is assigned to the body or to the wheel it wraps around.
//! Adapted from benilla-vehicles' `model.rs` (green-lambo.glb).

use std::collections::HashMap;

use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

const GLB: &[u8] = include_bytes!("../assets/car.glb");

/// The glb's mesh space under its Sketchfab root, whose 2° skew is left out: centimetres, +X front,
/// +Y left, +Z up, the rear tyres' bottom at Z −3.45.
const UNIT: f32 = 0.01;
const GROUND: f32 = -3.45;
/// Wheel centres and radii there: (x, z, r).
const FRONT: (f32, f32, f32) = (131.7, 39.6, 39.1);
const BACK: (f32, f32, f32) = (-157.7, 37.7, 41.1);
/// The tyres' inner face, |y|.
const TYRE_INNER: f32 = 58.0;
const HUB: f32 = 85.0;
/// A wheel piece wrapping less than this (rad, 200°) around its axle is held by the body.
const SPIN_SPAN: f32 = 3.49;
/// Pieces within this radius of the axle turn with the wheel whatever their shape.
const HUB_CORE: f32 = 6.0;
/// Opacity of the windows.
const GLASS: f32 = 0.35;

/// Model space: +Z front, +X left, +Y up, the ground at Y 0, in metres; [`CAR_SCALE`] takes it to yards.
pub const CAR_SCALE: f32 = 1.361;
/// The wheels' radius in yards, front then back averaged for the roll rate.
pub const WHEEL_RADIUS: f32 = 0.4 * CAR_SCALE;
/// Half the track and half the wheelbase in yards, where a tyre meets the road.
pub const SMOKE_X: f32 = 0.84 * CAR_SCALE;
pub const SMOKE_Z: f32 = 1.447 * CAR_SCALE;
/// Where the axles' midpoint is along model Z, which the car's centre is moved to.
pub const AXLE_MID: f32 = (1.317 + -1.577) * 0.5;

pub struct Part {
    pub kind: PartKind,
    pub finish: Finish,
    pub mesh: Mesh,
    /// The wheel's hub (zero for the body); the mesh is re-centred on it.
    pub pivot: Vec3,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum PartKind {
    Body,
    BackWheels,
    FrontLeft,
    FrontRight,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Finish {
    /// The material's colour alone.
    Plain,
    /// The material's colour over the glb's texture.
    Textured,
    Glass,
}

/// glb space to model space; the raw mesh is stored mirrored.
fn point(p: [f32; 3]) -> Vec3 {
    Vec3::new(-p[1], p[2] - GROUND, p[0]) * UNIT
}

/// The wheel a raw-space vertex is part of: inside a tyre's cylinder, outboard of its inner face.
fn wheel_of(p: [f32; 3]) -> PartKind {
    let inside = |(x, z, r): (f32, f32, f32)| {
        (p[0] - x).powi(2) + (p[2] - z).powi(2) <= (r + 1.5).powi(2) && p[1].abs() >= TYRE_INNER
    };
    if inside(BACK) {
        PartKind::BackWheels
    } else if inside(FRONT) {
        if p[1] < 0.0 {
            PartKind::FrontLeft
        } else {
            PartKind::FrontRight
        }
    } else {
        PartKind::Body
    }
}

/// Each vertex's part: a connected piece turns with a wheel only if it lies wholly in that wheel and
/// wraps around its axle, so the calipers, uprights and fender lips stay on the body.
fn parts_of(p: &[[f32; 3]], tris: &[u32]) -> Vec<PartKind> {
    let mut weld: HashMap<[u32; 3], usize> = HashMap::new();
    let node: Vec<usize> = p
        .iter()
        .map(|v| {
            let next = weld.len();
            *weld.entry(v.map(f32::to_bits)).or_insert(next)
        })
        .collect();
    let mut parent: Vec<usize> = (0..weld.len()).collect();
    fn root(parent: &mut [usize], mut a: usize) -> usize {
        while parent[a] != a {
            parent[a] = parent[parent[a]];
            a = parent[a];
        }
        a
    }
    for tri in tris.chunks_exact(3) {
        let a = root(&mut parent, node[tri[0] as usize]);
        for &i in &tri[1..] {
            let b = root(&mut parent, node[i as usize]);
            parent[b] = a;
        }
    }
    let piece: Vec<usize> = node.iter().map(|&n| root(&mut parent, n)).collect();

    // Per piece: its one wheel (None once it spans two parts), angles about that axle, reach.
    let mut pieces: HashMap<usize, (Option<PartKind>, Vec<f32>, f32)> = HashMap::new();
    for (v, &id) in p.iter().zip(&piece) {
        let kind = wheel_of(*v);
        let e = pieces.entry(id).or_insert((Some(kind), Vec::new(), 0.0));
        if e.0 != Some(kind) {
            e.0 = None;
        }
        if kind != PartKind::Body {
            let (x, z, _) = if kind == PartKind::BackWheels {
                BACK
            } else {
                FRONT
            };
            e.1.push((v[2] - z).atan2(v[0] - x));
            e.2 = e.2.max((v[0] - x).hypot(v[2] - z));
        }
    }
    let spins: HashMap<usize, PartKind> = pieces
        .into_iter()
        .filter_map(|(id, (kind, mut angles, reach))| {
            let kind = kind.filter(|k| *k != PartKind::Body)?;
            angles.sort_by(f32::total_cmp);
            let wrap = angles.first()? + std::f32::consts::TAU - angles.last()?;
            let gap = angles.windows(2).map(|w| w[1] - w[0]).fold(wrap, f32::max);
            let span = std::f32::consts::TAU - gap;
            (span >= SPIN_SPAN || reach <= HUB_CORE).then_some((id, kind))
        })
        .collect();
    piece
        .iter()
        .map(|id| spins.get(id).copied().unwrap_or(PartKind::Body))
        .collect()
}

#[derive(Default)]
struct Builder {
    pos: Vec<[f32; 3]>,
    nrm: Vec<[f32; 3]>,
    col: Vec<[f32; 4]>,
    uv: Vec<[f32; 2]>,
    idx: Vec<u32>,
}

fn mesh(b: Builder, pivot: Vec3) -> Mesh {
    let mut m = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    let n = b.pos.len();
    let pos: Vec<[f32; 3]> = b
        .pos
        .into_iter()
        .map(|p| (Vec3::from(p) - pivot).to_array())
        .collect();
    m.insert_attribute(Mesh::ATTRIBUTE_POSITION, pos);
    m.insert_attribute(Mesh::ATTRIBUTE_NORMAL, b.nrm);
    m.insert_attribute(Mesh::ATTRIBUTE_COLOR, b.col);
    if b.uv.len() == n && n > 0 {
        m.insert_attribute(Mesh::ATTRIBUTE_UV_0, b.uv);
    }
    m.insert_indices(Indices::U32(b.idx));
    m
}

/// The car's parts, split per wheel and finish, and the texture its textured parts use.
pub fn load() -> (Vec<Part>, Image) {
    let gltf = gltf::Gltf::from_slice(GLB).expect("embedded car glb parses");
    let blob = gltf.blob.as_deref().expect("car glb has a binary chunk");
    let mut builders: HashMap<(PartKind, Finish), Builder> = HashMap::new();
    let mut texture = None;
    for node_mesh in gltf.meshes() {
        for prim in node_mesh.primitives() {
            let reader = prim.reader(|b| match b.source() {
                gltf::buffer::Source::Bin => Some(blob),
                _ => None,
            });
            let material = prim.material();
            let pbr = material.pbr_metallic_roughness();
            let base = Vec4::from(pbr.base_color_factor());
            let finish = if base.w < 0.5 {
                Finish::Glass
            } else if let Some(info) = pbr.base_color_texture() {
                texture.get_or_insert(info.texture().source());
                Finish::Textured
            } else {
                Finish::Plain
            };
            let p: Vec<[f32; 3]> = reader.read_positions().into_iter().flatten().collect();
            let n: Vec<[f32; 3]> = reader.read_normals().into_iter().flatten().collect();
            let t: Vec<[f32; 2]> = reader
                .read_tex_coords(0)
                .map(|t| t.into_f32().collect())
                .unwrap_or_default();
            let tris: Vec<u32> = match reader.read_indices() {
                Some(ix) => ix.into_u32().collect(),
                None => (0..p.len() as u32).collect(),
            };
            let kinds = parts_of(&p, &tris);
            let mut remap: HashMap<(PartKind, u32), u32> = HashMap::new();
            for tri in tris.chunks_exact(3) {
                let kind = kinds[tri[0] as usize];
                let b = builders.entry((kind, finish)).or_default();
                for &i in tri {
                    let at = *remap.entry((kind, i)).or_insert_with(|| {
                        let raw = n.get(i as usize).copied().unwrap_or([0.0, 0.0, 1.0]);
                        let normal = Vec3::new(-raw[1], raw[2], raw[0]);
                        let alpha = if finish == Finish::Glass { GLASS } else { 1.0 };
                        b.pos.push(point(p[i as usize]).to_array());
                        b.nrm.push(normal.to_array());
                        b.col.push([base.x, base.y, base.z, alpha]);
                        b.uv.push(t.get(i as usize).copied().unwrap_or_default());
                        b.pos.len() as u32 - 1
                    });
                    b.idx.push(at);
                }
            }
        }
    }
    let pivot = |kind: PartKind| match kind {
        PartKind::Body => Vec3::ZERO,
        PartKind::BackWheels => point([BACK.0, 0.0, BACK.1]),
        PartKind::FrontLeft => point([FRONT.0, -HUB, FRONT.1]),
        PartKind::FrontRight => point([FRONT.0, HUB, FRONT.1]),
    };
    let mut keys: Vec<_> = builders.keys().copied().collect();
    // Wheels after the body, a wheel's parts together.
    keys.sort_by_key(|(k, f)| (*k as u8, *f as u8));
    let parts = keys
        .into_iter()
        .map(|key| {
            let b = builders.remove(&key).unwrap_or_default();
            let at = pivot(key.0);
            Part {
                kind: key.0,
                finish: key.1,
                mesh: mesh(b, at),
                pivot: at,
            }
        })
        .collect();
    let bytes = match texture.map(|image| image.source()) {
        Some(gltf::image::Source::View { view, .. }) => {
            &blob[view.offset()..view.offset() + view.length()]
        }
        _ => &[][..],
    };
    (parts, texture_of(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(parts: &[Part], kind: PartKind) -> usize {
        parts.iter().filter(|p| p.kind == kind).count()
    }

    #[test]
    fn wheels_are_split_from_the_body() {
        let (parts, _) = load();
        assert!(count(&parts, PartKind::Body) > 0);
        for wheel in [
            PartKind::BackWheels,
            PartKind::FrontLeft,
            PartKind::FrontRight,
        ] {
            assert!(count(&parts, wheel) > 0, "{wheel:?} has no parts");
        }
        // The wheels are small next to the body: a tyre, not the car, was cut out.
        let wheel_tris: usize = parts
            .iter()
            .filter(|p| p.kind != PartKind::Body)
            .map(|p| p.mesh.indices().map_or(0, |i| i.len()) / 3)
            .sum();
        let body_tris: usize = parts
            .iter()
            .filter(|p| p.kind == PartKind::Body)
            .map(|p| p.mesh.indices().map_or(0, |i| i.len()) / 3)
            .sum();
        assert!(
            wheel_tris > 100 && wheel_tris < body_tris,
            "{wheel_tris} vs {body_tris}"
        );
    }

    #[test]
    fn wheel_hubs_sit_at_the_axles() {
        let (parts, _) = load();
        let hub = |kind| parts.iter().find(|p| p.kind == kind).unwrap().pivot;
        // Metres; front axle +1.317 along Z, back -1.577, the left wheel on +X.
        assert!((hub(PartKind::FrontLeft).z - 1.317).abs() < 0.01);
        assert!((hub(PartKind::BackWheels).z + 1.577).abs() < 0.01);
        assert!(hub(PartKind::FrontLeft).x > 0.5 && hub(PartKind::FrontRight).x < -0.5);
    }
}

/// sRGB colour with its full mip chain, wrapping as the glb's sampler does.
fn texture_of(png: &[u8]) -> Image {
    let mut level = image::load_from_memory(png)
        .map(|i| i.to_rgba8())
        .unwrap_or_else(|_| image::RgbaImage::from_pixel(1, 1, image::Rgba([255; 4])));
    let (width, height) = level.dimensions();
    let mut data = Vec::new();
    let mut levels = 0;
    loop {
        data.extend_from_slice(level.as_raw());
        levels += 1;
        let (w, h) = level.dimensions();
        if w == 1 && h == 1 {
            break;
        }
        level = image::imageops::resize(
            &level,
            (w / 2).max(1),
            (h / 2).max(1),
            image::imageops::FilterType::Triangle,
        );
    }
    let mut image = Image::new_uninit(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.data = Some(data);
    image.texture_descriptor.mip_level_count = levels;
    image.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        mipmap_filter: ImageFilterMode::Linear,
        anisotropy_clamp: 8,
        ..Default::default()
    });
    image
}
