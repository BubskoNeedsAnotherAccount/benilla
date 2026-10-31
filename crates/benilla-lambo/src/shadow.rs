//! The car's shadow: the car's own outline cast along the world sun onto the ground under it,
//! soft at the edge, over a small contact blob where the body sits closest to the road.
//!
//! The car's model is reduced once to a cloud of outline points; each frame they are projected along
//! `WowLighting::sun_dir` onto the ground plane, and the 2D convex hull of the projection becomes a
//! fan with a feathered rim. Its darkness follows the time of day: a shadow takes away the sun's
//! diffuse share of the light, so it fades as the diffuse colour dims toward night.

use std::f32::consts::TAU;

use benilla_world::collision::WorldCollision;
use benilla_world::lighting::WowLighting;
use bevy::asset::RenderAssetUsages;
use bevy::camera::visibility::NoFrustumCulling;
use bevy::mesh::{Indices, PrimitiveTopology};
use bevy::prelude::*;

use crate::car::{Car, RIDE};

/// The shadow's colour; its alpha comes from the light.
const SHADE: [f32; 3] = [0.015, 0.015, 0.02];
/// Height of the shadow over the ground plane, against depth fighting with the terrain.
const LIFT: f32 = 0.08;
/// Height slices the model's outline is taken in.
const SLICES: usize = 14;
/// The darkest the cast shadow and the contact blob get.
const CAST_MAX: f32 = 0.62;
const CONTACT_MAX: f32 = 0.42;
/// Rim softness: a base width, and a widening per yard the light travels from the car to the ground.
const PENUMBRA_BASE: f32 = 0.14;
const PENUMBRA_PER_YARD: f32 = 0.07;
/// The sun when the world's lighting has not resolved yet: its 225° bearing, 30° up.
const FALLBACK_SUN: Vec3 = Vec3::new(0.612, -0.5, 0.612);
/// Points around the contact blob's rim.
const BLOB_SEGMENTS: usize = 28;

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

/// The shadow entity, with the ground plane it last lay on.
#[derive(Component, Default)]
pub struct CarShadow {
    plane: Option<(Vec3, Vec3)>,
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

/// Mesh buffers for one frame of the shadow.
#[derive(Default)]
struct Shape {
    pos: Vec<[f32; 3]>,
    col: Vec<[f32; 4]>,
    idx: Vec<u32>,
}

impl Shape {
    fn vertex(&mut self, p: Vec2, alpha: f32) -> u32 {
        self.pos.push([p.x, 0.0, p.y]);
        self.col.push([SHADE[0], SHADE[1], SHADE[2], alpha]);
        self.pos.len() as u32 - 1
    }

    /// A convex ring with a solid core and a rim fading out over `widths`, one per point.
    fn soft_polygon(&mut self, ring: &[Vec2], widths: &[f32], alpha: f32) {
        let n = ring.len();
        if n < 3 || alpha <= 0.0 {
            return;
        }
        let centre = ring.iter().copied().sum::<Vec2>() / n as f32;
        let c = self.vertex(centre, alpha);
        let base = self.pos.len() as u32;
        // Each point moves along its own ray from the centre: the rays of a convex ring never cross,
        // so no two pieces overlap and draw twice, a darker seam. The step is lengthened by how far
        // the ray leans off the edge normal, to keep the rim its width across the edge.
        for i in 0..n {
            let prev = ring[(i + n - 1) % n];
            let next = ring[(i + 1) % n];
            let radial = ring[i] - centre;
            let reach = radial.length();
            let dir = radial / reach.max(1e-4);
            // A counter-clockwise ring's outward normal is the edge's clockwise perpendicular.
            let normal = -((ring[i] - prev).perp().normalize_or_zero()
                + (next - ring[i]).perp().normalize_or_zero())
            .normalize_or(dir);
            let half = widths[i] * 0.5 / dir.dot(normal).max(0.4);
            self.vertex(centre + dir * (reach - half.min(reach * 0.5)), alpha);
            self.vertex(centre + dir * (reach + half), 0.0);
        }
        for i in 0..n as u32 {
            let j = (i + 1) % n as u32;
            let (in_i, out_i) = (base + 2 * i, base + 2 * i + 1);
            let (in_j, out_j) = (base + 2 * j, base + 2 * j + 1);
            self.idx.extend([c, in_j, in_i]);
            self.idx.extend([in_i, in_j, out_i, out_i, in_j, out_j]);
        }
    }

    fn into_mesh(self, mesh: &mut Mesh) {
        let normals = vec![[0.0, 1.0, 0.0]; self.pos.len()];
        mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, self.pos);
        mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
        mesh.insert_attribute(Mesh::ATTRIBUTE_COLOR, self.col);
        mesh.insert_indices(Indices::U32(self.idx));
    }
}

/// The contact blob: an oval over the footprint, darkest at its centre.
fn contact_blob(shape: &mut Shape, centre: Vec2, half: Vec2, alpha: f32) {
    let blob: Vec<Vec2> = (0..BLOB_SEGMENTS)
        .map(|i| {
            let a = TAU * i as f32 / BLOB_SEGMENTS as f32;
            centre + Vec2::new(a.cos(), a.sin()) * half
        })
        .collect();
    shape.soft_polygon(&blob, &[half.x * 0.9; BLOB_SEGMENTS], alpha);
}

/// The cast shadow: the hull of the outline carried to the ground, softer where the light
/// travelled further from the car.
fn cast_outline(shape: &mut Shape, cast: &[Vec2], travel: &[f32], spread: f32, alpha: f32) {
    let hull = convex_hull(cast);
    let ring: Vec<Vec2> = hull.iter().map(|&i| cast[i]).collect();
    let widths: Vec<f32> = hull
        .iter()
        .map(|&i| (PENUMBRA_BASE + PENUMBRA_PER_YARD * travel[i]) * spread)
        .collect();
    shape.soft_polygon(&ring, &widths, alpha);
}

pub fn setup_car_shadow(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    let mut mesh = Mesh::new(
        PrimitiveTopology::TriangleList,
        RenderAssetUsages::default(),
    );
    Shape::default().into_mesh(&mut mesh);
    let material = materials.add(StandardMaterial {
        base_color: Color::WHITE,
        alpha_mode: AlphaMode::Blend,
        unlit: true,
        cull_mode: None,
        ..default()
    });
    commands.spawn((
        CarShadow::default(),
        Mesh3d(meshes.add(mesh)),
        MeshMaterial3d(material),
        Transform::from_translation(Vec3::new(0.0, -9999.0, 0.0)),
        Visibility::Hidden,
        // The mesh is rebuilt every frame and its bounds are not.
        NoFrustumCulling,
    ));
}

#[allow(clippy::type_complexity)]
pub fn update_car_shadow(
    time: Res<Time>,
    world: WorldCollision,
    lighting: Option<Res<WowLighting>>,
    silhouette: Option<Res<CarSilhouette>>,
    car_q: Query<&Car>,
    mut shadow_q: Query<(&mut CarShadow, &mut Transform, &mut Visibility, &Mesh3d), Without<Car>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let (Ok(car), Ok((mut shadow, mut tf, mut vis, mesh))) =
        (car_q.single(), shadow_q.single_mut())
    else {
        return;
    };
    let Some(silhouette) = silhouette else {
        return;
    };
    if !car.placed {
        shadow.plane = None;
        vis.set_if_neq(Visibility::Hidden);
        return;
    }

    // The ground plane: under the wheels when they touch it, else the ground below the car.
    let (ground, normal, altitude) = if car.grounded {
        (car.pos - car.up * RIDE, car.up, 0.0)
    } else {
        let probe = car.pos + Vec3::Y * 1.5;
        let Some(hit) = world.ray_body(probe, Dir3::NEG_Y, 70.0) else {
            shadow.plane = None;
            vis.set_if_neq(Visibility::Hidden);
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
    let (ground, normal) = match shadow.plane {
        Some((g, n)) if !car.grounded => {
            let t = 1.0 - (-20.0 * time.delta_secs()).exp();
            (g.lerp(ground, t), n.lerp(normal, t).normalize_or(normal))
        }
        _ => (ground, normal),
    };
    shadow.plane = Some((ground, normal));

    let fade = (1.0 - altitude / 30.0).clamp(0.0, 1.0);
    if fade <= 0.0 {
        vis.set_if_neq(Visibility::Hidden);
        return;
    }

    let (sun, ambient, diffuse) = match lighting.as_deref() {
        Some(l) if l.sun_dir.length_squared() > 0.5 => (l.sun_dir, l.ambient, l.diffuse),
        _ => (FALLBACK_SUN.normalize(), [0.4; 3], [0.6; 3]),
    };
    let ray = cast_direction(sun, normal);

    // The plane's frame, aligned with the car's heading: x right, z back.
    let (fwd, _) = car.basis();
    let flat_fwd = (fwd - normal * fwd.dot(normal)).normalize_or(fwd);
    let right = flat_fwd.cross(normal).normalize_or(Vec3::X);
    let origin = ground + normal * LIFT;
    tf.translation = origin;
    tf.rotation = Quat::from_mat3(&Mat3::from_cols(right, normal, -flat_fwd));
    tf.scale = Vec3::ONE;
    let to_plane = |p: Vec3| Vec2::new((p - origin).dot(right), -(p - origin).dot(flat_fwd));

    // Each outline point carried along the light to the plane, with how far it travelled.
    let rot = car.rotation();
    let (cast, travel): (Vec<Vec2>, Vec<f32>) = silhouette
        .points
        .iter()
        .map(|&local| {
            let (p, t) = project(car.pos + rot * local, origin, normal, ray);
            (to_plane(p), t)
        })
        .unzip();

    let mut shape = Shape::default();

    // The contact blob: the car's footprint, darkest at its centre, spreading as the car climbs.
    let spread = 1.0 + (altitude / 25.0).min(0.6);
    let footprint = silhouette.centre;
    let centre = to_plane(car.pos + rot * Vec3::new(footprint.x, 0.0, footprint.y));
    let half = silhouette.half * Vec2::new(0.95, 0.9) * spread;
    contact_blob(&mut shape, centre, half, CONTACT_MAX * fade * fade);

    // The cast shadow, softer where the light travelled further from the car.
    let strength = cast_strength(ambient, diffuse, sun, normal);
    cast_outline(
        &mut shape,
        &cast,
        &travel,
        spread,
        CAST_MAX * strength * fade,
    );

    if let Some(mesh) = meshes.get_mut(&mesh.0) {
        shape.into_mesh(mesh);
    }
    vis.set_if_neq(Visibility::Visible);
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

    /// How much the shape's triangles cover beyond the area they outline: zero when none overlap,
    /// and an overlap draws twice, a darker seam.
    fn overlap(shape: &Shape) -> f32 {
        let p: Vec<Vec2> = shape.pos.iter().map(|v| Vec2::new(v[0], v[2])).collect();
        let tris: f32 = shape
            .idx
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let (a, b, c) = (p[t[0] as usize], p[t[1] as usize], p[t[2] as usize]);
                (b - a).perp_dot(c - a).abs() * 0.5
            })
            .sum();
        let hull = convex_hull(&p);
        let outline: f32 = (0..hull.len())
            .map(|i| p[hull[i]].perp_dot(p[hull[(i + 1) % hull.len()]]) * 0.5)
            .sum();
        tris - outline
    }

    #[test]
    fn the_shadow_never_overlaps_itself() {
        let (parts, _) = crate::model::load();
        let s = crate::car::silhouette(&parts);
        let origin = Vec3::new(0.0, -RIDE + LIFT, 0.0);
        let mut worst = (0.0f32, 0.0f32, 0.0f32);
        for elev in [20.0f32, 30.0, 37.0, 60.0, 90.0] {
            for az in (0..24).map(|i| i as f32 * 15.0) {
                let (e, a) = (elev.to_radians(), az.to_radians());
                let sun = Vec3::new(e.cos() * a.cos(), -e.sin(), e.cos() * a.sin());
                let (cast, travel): (Vec<Vec2>, Vec<f32>) = s
                    .points
                    .iter()
                    .map(|&p| {
                        let (q, t) = project(p, origin, Vec3::Y, sun);
                        (Vec2::new(q.x, q.z), t)
                    })
                    .unzip();
                // On the ground and at the widest airborne spread.
                for spread in [1.0, 1.6] {
                    let mut shape = Shape::default();
                    cast_outline(&mut shape, &cast, &travel, spread, 0.5);
                    let excess = overlap(&shape);
                    if excess > worst.0 {
                        worst = (excess, elev, az);
                    }
                }
            }
        }
        let mut blob = Shape::default();
        contact_blob(&mut blob, Vec2::ZERO, s.half, 0.4);
        assert!(
            overlap(&blob) < 1e-3,
            "blob overlaps by {} yd²",
            overlap(&blob)
        );
        assert!(
            worst.0 < 1e-3,
            "cast shadow overlaps by {} yd² at elevation {}°, azimuth {}°",
            worst.0,
            worst.1,
            worst.2
        );
    }
}
