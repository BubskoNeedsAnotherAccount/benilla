//! The car's shadow: the car's own outline cast along the world sun, laid on the ground under it
//! through the engine's ground-decal projector, over a soft contact blob under the body.
//!
//! The car's model is reduced once to a cloud of outline points. Each frame they are carried along
//! `WowLighting::sun_dir` onto the ground plane under the car, the convex hull of where they land is
//! drawn with a soft rim into a small texture, and [`WorldDecal`] clips the terrain and WMO
//! triangles under it, so the shadow lies on the ground itself, slopes and bumps included, as the
//! unit blob shadow does. It multiplies into the ground on the blob shadow's rung. Its darkness
//! follows the time of day: a shadow takes away the sun's diffuse share of the light, so it fades
//! as the diffuse colour dims toward night.

use benilla_world::collision::WorldCollision;
use benilla_world::decal::{DecalFrame, WorldDecal};
use benilla_world::lighting::WowLighting;
use benilla_world::particles::buffer::{EffectVertex, WorldEffectDraw};
use benilla_world::sky_order::Rung;
use benilla_world::view::WorldCamera;
use bevy::asset::RenderAssetUsages;
use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::car::{Car, RIDE};

/// The shadow's colour, raw bytes: the multiply takes the ground toward it by the texel's alpha.
const SHADE: [u8; 3] = [4, 4, 5];
/// The shadow texture's side in texels; the decal box is stretched over it.
const TEX: usize = 128;
/// Height slices the model's outline is taken in.
const SLICES: usize = 14;
/// The darkest the cast shadow and the contact blob get.
const CAST_MAX: f32 = 0.62;
const CONTACT_MAX: f32 = 0.42;
/// Rim softness: a base width, and a widening per yard the light travels from the car to the ground.
const PENUMBRA_BASE: f32 = 0.14;
const PENUMBRA_PER_YARD: f32 = 0.07;
/// How far off the ground plane a receiving surface still takes the full shadow, and over how much
/// more it fades out: the ground under a bump keeps it, a terrace far below a ledge does not.
const PLANE_HOLD: f32 = 1.0;
const PLANE_FADE: f32 = 1.5;
/// The sun when the world's lighting has not resolved yet: its 225° bearing, 30° up.
const FALLBACK_SUN: Vec3 = Vec3::new(0.612, -0.5, 0.612);

/// The car's outline points in its own space (+Y up, −Z forward, yards, the body centre at the origin),
/// and its footprint's centre and half extents on (x, z).
#[derive(Resource)]
pub struct CarSilhouette {
    pub points: Vec<Vec3>,
    pub centre: Vec2,
    pub half: Vec2,
}

impl CarSilhouette {
    /// Reduces the model's vertices, already in car space, to the outline of each height slice: the
    /// points the shadow's outline can come from.
    pub fn from_points(all: &[Vec3]) -> Self {
        let (lo, hi) = all.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| {
            (lo.min(p.y), hi.max(p.y))
        });
        let step = ((hi - lo) / SLICES as f32).max(1e-3);
        let mut slices: Vec<Vec<Vec3>> = vec![Vec::new(); SLICES];
        for p in all {
            let i = (((p.y - lo) / step) as usize).min(SLICES - 1);
            slices[i].push(*p);
        }
        let mut points = Vec::new();
        for slice in &slices {
            let flat: Vec<Vec2> = slice.iter().map(|p| Vec2::new(p.x, p.z)).collect();
            points.extend(convex_hull(&flat).into_iter().map(|i| slice[i]));
        }
        let (lo, hi) = points.iter().fold((Vec2::MAX, Vec2::MIN), |(lo, hi), p| {
            let f = Vec2::new(p.x, p.z);
            (lo.min(f), hi.max(f))
        });
        Self {
            points,
            centre: (lo + hi) * 0.5,
            half: (hi - lo) * 0.5,
        }
    }
}

/// The shadow's state: the ground plane it last lay on, its texture, and this frame's projected
/// triangles (empty when hidden).
#[derive(Resource)]
pub struct CarShadow {
    plane: Option<(Vec3, Vec3)>,
    texture: Handle<Image>,
    verts: Vec<EffectVertex>,
    anchor: Vec3,
    /// The draw's owner, for the render world's bookkeeping.
    owner: Entity,
}

/// Indices of the convex hull of `pts`, counter-clockwise in (x, y), by the monotone chain.
pub fn convex_hull(pts: &[Vec2]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..pts.len()).collect();
    order.sort_by(|&a, &b| {
        pts[a]
            .x
            .total_cmp(&pts[b].x)
            .then(pts[a].y.total_cmp(&pts[b].y))
    });
    order.dedup_by(|a, b| pts[*a] == pts[*b]);
    if order.len() < 3 {
        return order;
    }
    let cross = |o: Vec2, a: Vec2, b: Vec2| (a - o).perp_dot(b - o);
    let mut hull: Vec<usize> = Vec::with_capacity(order.len() + 1);
    for pass in [false, true] {
        let start = hull.len();
        let iter: Box<dyn Iterator<Item = &usize>> = if pass {
            Box::new(order.iter().rev())
        } else {
            Box::new(order.iter())
        };
        for &i in iter {
            while hull.len() >= start + 2
                && cross(pts[hull[hull.len() - 2]], pts[hull[hull.len() - 1]], pts[i]) <= 0.0
            {
                hull.pop();
            }
            hull.push(i);
        }
        hull.pop();
    }
    hull
}

/// The ground's share of light the shadow takes away: the sun's diffuse term over the whole, from
/// the world's ambient and diffuse colours, the ground normal and the sun's travel direction.
fn cast_strength(ambient: [f32; 3], diffuse: [f32; 3], sun: Vec3, normal: Vec3) -> f32 {
    let luma = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    let lit = luma(diffuse) * (-sun.dot(normal)).max(0.0);
    (lit / (luma(ambient) + lit + 1e-4)).clamp(0.0, 1.0)
}

/// The sun direction to cast along, kept at least this steep against the ground so a grazing sun on
/// a slope does not stretch the shadow out to the horizon.
fn cast_direction(sun: Vec3, normal: Vec3) -> Vec3 {
    const MIN_DOT: f32 = 0.3;
    let d = sun.dot(normal);
    if d > -MIN_DOT {
        let along = (sun - normal * d).normalize_or_zero();
        along * (1.0 - MIN_DOT * MIN_DOT).sqrt() - normal * MIN_DOT
    } else {
        sun
    }
}

/// `p` carried along `ray` onto the plane through `origin` with `normal`, and how far it went; a
/// point already under the plane stays where it is. `ray` must point into the plane.
fn project(p: Vec3, origin: Vec3, normal: Vec3, ray: Vec3) -> (Vec3, f32) {
    let t = ((p - origin).dot(normal) / -ray.dot(normal)).max(0.0);
    (p + ray * t, t)
}

/// One frame's shadow in the decal frame's horizontal coordinates: the cast hull with a rim width
/// per point, and the contact blob.
struct Footprint {
    ring: Vec<Vec2>,
    widths: Vec<f32>,
    cast_alpha: f32,
    blob_centre: Vec2,
    blob_half: Vec2,
}

impl Footprint {
    /// The box the shadow fits in, with room for the rim to fade out before the texture's edge.
    fn bounds(&self) -> (Vec2, Vec2) {
        let reach = self.blob_half * 1.1;
        let (mut lo, mut hi) = (self.blob_centre - reach, self.blob_centre + reach);
        for p in &self.ring {
            (lo, hi) = (lo.min(*p), hi.max(*p));
        }
        let margin = self.widths.iter().copied().fold(0.0, f32::max) * 0.6 + 0.2;
        (lo - margin, hi + margin)
    }

    /// Draws the shadow over `[lo, hi]` into an RGBA `TEX × TEX` texture: the hull with its rim
    /// centred on the outline, the blob darkest at its centre, the two darkening together.
    fn raster(&self, lo: Vec2, hi: Vec2, out: &mut [u8]) {
        // Each edge as (a point on it, outward normal over its rim width): a texel's largest
        // `(p − a)·k` is how far out it lies in rim widths, negative inside.
        let n = self.ring.len();
        let edges: Vec<(Vec2, Vec2)> = if n >= 3 {
            (0..n)
                .map(|i| {
                    let (a, b) = (self.ring[i], self.ring[(i + 1) % n]);
                    // A counter-clockwise ring's outward normal is the edge's clockwise perpendicular.
                    let normal = -(b - a).perp().normalize_or_zero();
                    let width = ((self.widths[i] + self.widths[(i + 1) % n]) * 0.5).max(1e-3);
                    (a, normal / width)
                })
                .collect()
        } else {
            Vec::new()
        };
        let step = (hi - lo) / TEX as f32;
        for (k, texel) in out
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .enumerate()
            .take(TEX * TEX)
        {
            let p = lo + (Vec2::new((k % TEX) as f32, (k / TEX) as f32) + 0.5) * step;
            let cast = if edges.is_empty() {
                0.0
            } else {
                let out = edges
                    .iter()
                    .fold(f32::MIN, |m, (a, k)| m.max((p - *a).dot(*k)));
                self.cast_alpha * (1.0 - smoothstep(-0.5, 0.5, out))
            };
            let r = ((p - self.blob_centre) / self.blob_half.max(Vec2::splat(1e-3))).length();
            let blob = CONTACT_MAX * (1.0 - smoothstep(0.3, 1.05, r));
            let alpha = 1.0 - (1.0 - cast) * (1.0 - blob);
            texel[..3].copy_from_slice(&SHADE);
            texel[3] = (alpha * 255.0).round() as u8;
        }
    }
}

/// A point as the decal projector hands it over, `(x', y − centre.y, z')` in a frame turned by
/// `(sin, cos)`, back to its world offset from the frame's centre: `DecalFrame`'s map inverted.
fn from_frame(p: Vec3, sin: f32, cos: f32) -> Vec3 {
    Vec3::new(p.x * cos + p.z * sin, p.y, p.z * cos - p.x * sin)
}

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn setup_car_shadow(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new_fill(
        Extent3d {
            width: TEX as u32,
            height: TEX as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 0],
        // Raw bytes, as the unit blob shadow's texture: the multiply runs on them unconverted.
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
    let owner = commands.spawn(Name::new("car-shadow")).id();
    commands.insert_resource(CarShadow {
        plane: None,
        texture: images.add(image),
        verts: Vec::new(),
        anchor: Vec3::ZERO,
        owner,
    });
}

#[allow(clippy::too_many_arguments)]
pub fn update_car_shadow(
    time: Res<Time>,
    world: WorldCollision,
    decals: WorldDecal,
    lighting: Option<Res<WowLighting>>,
    silhouette: Option<Res<CarSilhouette>>,
    car_q: Query<&Car>,
    shadow: Option<ResMut<CarShadow>>,
    mut images: ResMut<Assets<Image>>,
) {
    let (Ok(car), Some(silhouette), Some(mut shadow)) = (car_q.single(), silhouette, shadow) else {
        return;
    };
    let shadow = &mut *shadow;
    shadow.verts.clear();
    if !car.placed {
        shadow.plane = None;
        return;
    }

    // The ground plane: under the wheels when they touch it, else the ground below the car.
    let (ground, normal, altitude) = if car.grounded {
        (car.pos - car.up * RIDE, car.up, 0.0)
    } else {
        let probe = car.pos + Vec3::Y * 1.5;
        let Some(hit) = world.ray_body(probe, Dir3::NEG_Y, 70.0) else {
            shadow.plane = None;
            return;
        };
        let point = probe - Vec3::Y * hit.distance;
        let normal = if hit.normal.y > 0.15 {
            hit.normal.normalize_or(Vec3::Y)
        } else {
            Vec3::Y
        };
        (point, normal, (car.pos.y - point.y).max(0.0))
    };
    // In the air the probe steps across triangles, so the plane is eased; on the wheels it is exact.
    let (origin, normal) = match shadow.plane {
        Some((g, n)) if !car.grounded => {
            let t = 1.0 - (-20.0 * time.delta_secs()).exp();
            (g.lerp(ground, t), n.lerp(normal, t).normalize_or(normal))
        }
        _ => (ground, normal),
    };
    shadow.plane = Some((origin, normal));

    let fade = (1.0 - altitude / 30.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        return;
    }

    let (sun, ambient, diffuse) = match lighting.as_deref() {
        Some(l) if l.sun_dir.length_squared() > 0.5 => (l.sun_dir, l.ambient, l.diffuse),
        _ => (FALLBACK_SUN.normalize(), [0.4; 3], [0.6; 3]),
    };
    let ray = cast_direction(sun, normal);

    // The decal frame turns with the car, so the box hugs it; `in_frame` is `DecalFrame`'s own map.
    let (sin, cos) = car.yaw.sin_cos();
    let in_frame = |p: Vec3| {
        let (dx, dz) = (p.x - origin.x, p.z - origin.z);
        Vec2::new(dx * cos - dz * sin, dz * cos + dx * sin)
    };

    // Each outline point carried along the light to the plane: where it lands, how high that is
    // over the plane's origin, and how far the light travelled.
    let rot = car.rotation();
    let (mut low, mut high) = (0.0f32, 0.0f32);
    let mut cast = Vec::with_capacity(silhouette.points.len());
    let mut travel = Vec::with_capacity(silhouette.points.len());
    for &local in &silhouette.points {
        let (p, t) = project(car.pos + rot * local, origin, normal, ray);
        (low, high) = (low.min(p.y - origin.y), high.max(p.y - origin.y));
        cast.push(in_frame(p));
        travel.push(t);
    }

    // The cast hull, softer where the light travelled further, and the contact blob over the
    // footprint, both spreading as the car climbs.
    let spread = 1.0 + (altitude / 25.0).min(0.6);
    let hull = convex_hull(&cast);
    let footprint = silhouette.centre;
    let shape = Footprint {
        ring: hull.iter().map(|&i| cast[i]).collect(),
        widths: hull
            .iter()
            .map(|&i| (PENUMBRA_BASE + PENUMBRA_PER_YARD * travel[i]) * spread)
            .collect(),
        cast_alpha: CAST_MAX * cast_strength(ambient, diffuse, sun, normal),
        blob_centre: in_frame(car.pos + rot * Vec3::new(footprint.x, 0.0, footprint.y)),
        blob_half: silhouette.half * Vec2::new(0.95, 0.9) * spread,
    };
    let (lo, hi) = shape.bounds();
    let Some(data) = images
        .get_mut(&shadow.texture)
        .and_then(|image| image.data.as_mut())
    else {
        return;
    };
    shape.raster(lo, hi, data);

    // The terrain and WMO faces in the box, the ones far off the plane faded out.
    let frame = DecalFrame {
        center: origin,
        sin,
        cos,
        min_x: lo.x,
        max_x: hi.x,
        min_z: lo.y,
        max_z: hi.y,
        min_y: low - PLANE_HOLD - PLANE_FADE,
        max_y: high + PLANE_HOLD + PLANE_FADE,
    };
    let off_plane = |p: Vec3| {
        let h = from_frame(p, sin, cos).dot(normal).abs();
        (1.0 - (h - PLANE_HOLD) / PLANE_FADE).clamp(0.0, 1.0)
    };
    // White vertices with the fades in alpha; the colour is the texture's.
    let mut verts = std::mem::take(&mut shadow.verts);
    let landed = decals.project(
        &mut verts,
        &frame,
        |p| fade * off_plane(p),
        |x, z| frame.rect_uv(x, z),
    );
    if !landed {
        verts.clear();
    }
    shadow.verts = verts;
    shadow.anchor = origin;
}

/// Copies this frame's shadow onto the effect stream: a multiply on the unit blob shadow's rung.
pub fn push_car_shadow(
    shadow: Option<Res<CarShadow>>,
    cam: Query<Entity, With<WorldCamera>>,
    mut draw: WorldEffectDraw,
) {
    let (Some(shadow), Ok(cam)) = (shadow, cam.single()) else {
        return;
    };
    if shadow.verts.is_empty() {
        return;
    }
    let mut batch = draw
        .batch(cam, shadow.texture.id())
        .multiply()
        .anchored(shadow.anchor)
        .rung(Rung::SHADOW_SORT, Rung::DECAL_RASTER)
        .owner(shadow.owner);
    batch.vertices(&shadow.verts);
    batch.tris();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hull_of_a_square_with_inner_points() {
        let pts = [
            Vec2::new(0.0, 0.0),
            Vec2::new(1.0, 0.0),
            Vec2::new(0.5, 0.5),
            Vec2::new(1.0, 1.0),
            Vec2::new(0.0, 1.0),
            Vec2::new(0.2, 0.7),
        ];
        let hull = convex_hull(&pts);
        assert_eq!(hull.len(), 4);
        assert!(!hull.contains(&2) && !hull.contains(&5));
        // Counter-clockwise: positive signed area.
        let area: f32 = (0..hull.len())
            .map(|i| pts[hull[i]].perp_dot(pts[hull[(i + 1) % hull.len()]]))
            .sum();
        assert!(area > 0.0);
    }

    #[test]
    fn silhouette_keeps_the_outline() {
        // A 2 × 1 × 4 box of points, plus points inside it.
        let mut pts = Vec::new();
        for x in [-1.0, 1.0] {
            for y in [0.0, 1.0] {
                for z in [-2.0, 2.0] {
                    pts.push(Vec3::new(x, y, z));
                }
            }
        }
        pts.push(Vec3::new(0.0, 0.0, 0.0));
        let s = CarSilhouette::from_points(&pts);
        assert_eq!((s.centre, s.half), (Vec2::ZERO, Vec2::new(1.0, 2.0)));
        assert!(!s.points.contains(&Vec3::ZERO));
    }

    #[test]
    fn shadow_follows_the_light() {
        let normal = Vec3::Y;
        let overhead = cast_strength([0.4; 3], [0.6; 3], Vec3::NEG_Y, normal);
        let night = cast_strength([0.4; 3], [0.05; 3], Vec3::NEG_Y, normal);
        assert!(overhead > 0.5 && night < 0.15);
        // A grazing sun is steepened, never cast along the ground.
        let ray = cast_direction(Vec3::new(1.0, -0.01, 0.0).normalize(), normal);
        assert!(-ray.dot(normal) >= 0.29);
    }

    #[test]
    fn the_car_outline_matches_its_size() {
        let (parts, _) = crate::model::load();
        let s = crate::car::silhouette(&parts);
        // The car is about 2.4 m wide at the mirrors and 5.3 m long (the model runs about 7% large) at 1.361 yd/m, and stands on the ground at −RIDE.
        assert!((1.2..1.8).contains(&s.half.x), "half width {}", s.half.x);
        assert!((3.2..3.8).contains(&s.half.y), "half length {}", s.half.y);
        let low = s.points.iter().map(|p| p.y).fold(f32::MAX, f32::min);
        assert!((low + RIDE).abs() < 0.1, "lowest point {low}");
        assert!(
            (50..2000).contains(&s.points.len()),
            "{} points",
            s.points.len()
        );
    }

    #[test]
    fn a_roof_point_lands_away_from_the_sun() {
        // The sun 30° up, shining toward +X: a point 1.6 yd up lands 1.6 / tan 30° down-sun.
        let sun = Vec3::new(30f32.to_radians().cos(), -30f32.to_radians().sin(), 0.0);
        let (p, t) = project(Vec3::new(0.0, 1.6, 0.0), Vec3::ZERO, Vec3::Y, sun);
        assert!(p.y.abs() < 1e-5);
        assert!((p.x - 1.6 / 30f32.to_radians().tan()).abs() < 1e-4, "{p}");
        assert!((t - 3.2).abs() < 1e-4);
    }

    #[test]
    fn the_texture_holds_the_shadow_and_clears_its_edge() {
        // A 2 × 4 yd box shadow with the blob over it.
        let shape = Footprint {
            ring: vec![
                Vec2::new(-1.0, -2.0),
                Vec2::new(1.0, -2.0),
                Vec2::new(1.0, 2.0),
                Vec2::new(-1.0, 2.0),
            ],
            widths: vec![0.3; 4],
            cast_alpha: 0.5,
            blob_centre: Vec2::ZERO,
            blob_half: Vec2::new(1.0, 2.0),
        };
        let (lo, hi) = shape.bounds();
        let mut tex = vec![0u8; TEX * TEX * 4];
        shape.raster(lo, hi, &mut tex);
        let alpha = |x: usize, y: usize| tex[(y * TEX + x) * 4 + 3];
        // Centre: both, 1 − 0.5 · (1 − 0.42).
        assert_eq!(alpha(TEX / 2, TEX / 2), (0.71f32 * 255.0).round() as u8);
        // Every texel on the border is clear, so the clamped sampler smears nothing.
        for i in 0..TEX {
            for (x, y) in [(i, 0), (i, TEX - 1), (0, i), (TEX - 1, i)] {
                assert_eq!(alpha(x, y), 0, "border texel ({x}, {y})");
            }
        }
        // The counter-clockwise ring is dark inside, not out: a mirrored normal would invert it.
        assert!(alpha(TEX / 2, TEX / 8) > 0);
    }

    #[test]
    fn frame_coordinates_turn_back_to_world() {
        let (sin, cos) = 0.7f32.sin_cos();
        let world = Vec3::new(3.0, -0.4, -1.5);
        // `DecalFrame`'s own map, as `update_car_shadow`'s `in_frame` writes it.
        let framed = Vec3::new(
            world.x * cos - world.z * sin,
            world.y,
            world.z * cos + world.x * sin,
        );
        assert!(from_frame(framed, sin, cos).distance(world) < 1e-5);
    }
}
