//! The car: a procedural low-poly Lamborghini and its arcade physics.
//!
//! The engine runs no dynamics (`WorldPlugins` disables avian's solver and force plugins), so the
//! car integrates itself: four wheel rays against the streamed terrain and building colliders fit
//! a ground plane, a spring holds the body over it, and a bicycle model steers it. All lengths are
//! yards, as in the world.

use std::f32::consts::FRAC_PI_2;

use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::collision::WorldCollision;
use benilla_world::terrain_stream::ViewFocus;
use benilla_world::view::Viewer;

/// The Crossroads, in WoW coordinates (x north, y west), at a height above any ground there: the
/// car waits in the air for the terrain under it to stream in, then drops onto it.
const SPAWN_WOW: [f32; 3] = [-437.0, -2596.0, 400.0];

/// Half the track and wheelbase, to the wheel centres.
const WHEEL_X: f32 = 0.88;
const WHEEL_Z: f32 = 1.38;
const WHEEL_R: f32 = 0.34;
/// Body centre height above the ground when the wheels rest on it.
const RIDE: f32 = 0.48;
/// Front axle to rear axle.
const WHEELBASE: f32 = 2.76;

/// Spring stiffness and damping of the body over the ground plane.
const SPRING_K: f32 = 180.0;
const SPRING_C: f32 = 24.0;
/// The WoW gravity the world's physics is set to (`world_plugins`), in yd/s².
const GRAVITY: f32 = 19.291_105;
/// Lowest the body centre may sink toward the ground plane.
const MIN_HEIGHT: f32 = 0.44;

const ENGINE_ACCEL: f32 = 24.0;
const BRAKE_DECEL: f32 = 45.0;
const REVERSE_ACCEL: f32 = 12.0;
const REVERSE_MAX: f32 = 22.0;
/// Top speed in yd/s (about 280 km/h); boost raises both it and the pull.
pub const TOP_SPEED: f32 = 95.0;
const BOOST: f32 = 1.5;
const GRIP: f32 = 7.0;
const GRIP_HANDBRAKE: f32 = 1.2;

/// Physics steps per frame, so a fast car's wheel rays do not skip a bump.
const SUBSTEPS: u32 = 2;
/// The car's half length, for the wall probe.
const NOSE: f32 = 2.3;

/// The car's state; the entity's `Transform` is written from it each frame.
#[derive(Component)]
pub struct Car {
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    /// The smoothed ground normal the body rides on.
    pub up: Vec3,
    /// Front wheel angle in radians, positive to the right.
    pub steer: f32,
    /// Wheel roll angle, for the visuals.
    pub spin: f32,
    /// The car has found the ground since it was last (re)placed.
    placed: bool,
    /// Seconds with no wheel ray finding anything, as when driving ahead of the streamer.
    lost: f32,
}

impl Car {
    fn new() -> Self {
        Self {
            pos: wow_to_bevy(SPAWN_WOW),
            vel: Vec3::ZERO,
            yaw: 0.0,
            up: Vec3::Y,
            steer: 0.0,
            spin: 0.0,
            placed: false,
            lost: 0.0,
        }
    }

    /// The speed over the ground in yd/s.
    pub fn speed(&self) -> f32 {
        self.vel.length()
    }

    /// The heading on the ground plane: forward and right.
    pub fn basis(&self) -> (Vec3, Vec3) {
        basis(self.yaw, self.up)
    }

    fn rotation(&self) -> Quat {
        let (f, r) = self.basis();
        Quat::from_mat3(&Mat3::from_cols(r, self.up, -f))
    }
}

/// Forward (−Z at yaw 0, turning left as yaw grows) and right, on the plane with normal `up`.
fn basis(yaw: f32, up: Vec3) -> (Vec3, Vec3) {
    let flat = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let f = (flat - up * flat.dot(up)).normalize_or(flat);
    (f, f.cross(up))
}

/// `1 - e^(-rate·dt)`: a frame-rate independent smoothing factor.
pub fn smooth(dt: f32, rate: f32) -> f32 {
    1.0 - (-rate * dt).exp()
}

/// A wheel's visual: front wheels also take the steering angle.
#[derive(Component)]
pub struct Wheel {
    front: bool,
}

/// The driver's inputs for a frame.
struct Input {
    throttle: f32,
    steer: f32,
    handbrake: bool,
    boost: bool,
}

impl Input {
    fn read(keys: &ButtonInput<KeyCode>) -> Self {
        let axis = |pos: &[KeyCode], neg: &[KeyCode]| {
            let on = |set: &[KeyCode]| set.iter().any(|k| keys.pressed(*k));
            f32::from(u8::from(on(pos))) - f32::from(u8::from(on(neg)))
        };
        Self {
            throttle: axis(&[KeyCode::KeyW, KeyCode::ArrowUp], &[KeyCode::KeyS, KeyCode::ArrowDown]),
            steer: axis(
                &[KeyCode::KeyD, KeyCode::ArrowRight],
                &[KeyCode::KeyA, KeyCode::ArrowLeft],
            ),
            handbrake: keys.pressed(KeyCode::Space),
            boost: keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight),
        }
    }
}

/// Configuration for loading a 3D GLB model for the car.
#[derive(Resource, Clone, Debug, Default)]
pub struct LamboModelConfig {
    pub model_path: Option<String>,
}

/// Spawns the car: either a loaded GLB 3D model, or the procedural fallback body of coloured
/// boxes and four wheels.
pub fn spawn_car(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    model_config: Res<LamboModelConfig>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    if let Some(ref model_name) = model_config.model_path {
        let scale = std::env::var("LAMBO_SCALE")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(1.0);
        let yaw_deg = std::env::var("LAMBO_YAW")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.0);
        let offset = std::env::var("LAMBO_OFFSET")
            .ok()
            .and_then(|s| {
                let mut parts = s.split(',').filter_map(|p| p.trim().parse::<f32>().ok());
                Some(Vec3::new(parts.next()?, parts.next()?, parts.next()?))
            })
            .unwrap_or(Vec3::ZERO);

        let keep_wheels = std::env::var("LAMBO_WHEELS")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let scene_url = format!("{model_name}#Scene0");
        let scene: Handle<Scene> = asset_server.load(scene_url);

        commands
            .spawn((
                Car::new(),
                Transform::from_translation(wow_to_bevy(SPAWN_WOW)),
                Visibility::default(),
            ))
            .with_children(|car| {
                car.spawn((
                    SceneRoot(scene),
                    Transform::from_translation(offset)
                        .with_rotation(Quat::from_rotation_y(yaw_deg.to_radians()))
                        .with_scale(Vec3::splat(scale)),
                ));

                if keep_wheels {
                    let tyre = materials.add(StandardMaterial {
                        base_color: Color::srgb(0.015, 0.015, 0.015),
                        unlit: true,
                        ..default()
                    });
                    let wheel_mesh = meshes.add(Cylinder::new(WHEEL_R, 0.3));
                    for (sx, sz) in WHEELS {
                        car.spawn((
                            Wheel { front: sz < 0.0 },
                            Mesh3d(wheel_mesh.clone()),
                            MeshMaterial3d(tyre.clone()),
                            Transform::from_xyz(sx * WHEEL_X, -(RIDE - WHEEL_R), sz * WHEEL_Z),
                        ));
                    }
                }
            });
        return;
    }

    let mut mat = |r: f32, g: f32, b: f32| {
        materials.add(StandardMaterial {
            base_color: Color::srgb(r, g, b),
            unlit: true,
            ..default()
        })
    };
    let orange = mat(1.0, 0.45, 0.02);
    let dark = mat(0.03, 0.035, 0.05);
    let tyre = mat(0.015, 0.015, 0.015);
    let lamp = mat(1.0, 0.95, 0.75);
    let tail = mat(0.9, 0.02, 0.02);

    let mut part = |size: Vec3, at: Vec3, tilt: f32, material: &Handle<StandardMaterial>| {
        (
            Mesh3d(meshes.add(Cuboid::new(size.x, size.y, size.z))),
            MeshMaterial3d(material.clone()),
            Transform::from_translation(at).with_rotation(Quat::from_rotation_x(tilt)),
        )
    };
    let parts = [
        // Lower body, the hood sloping to the nose, the cabin, its roof, the splitter.
        part(Vec3::new(1.95, 0.42, 4.5), Vec3::new(0.0, 0.02, 0.0), 0.0, &orange),
        part(Vec3::new(1.85, 0.12, 1.7), Vec3::new(0.0, 0.28, -1.35), -0.12, &orange),
        part(Vec3::new(1.5, 0.38, 1.6), Vec3::new(0.0, 0.5, 0.25), 0.0, &dark),
        part(Vec3::new(1.4, 0.08, 1.0), Vec3::new(0.0, 0.72, 0.3), 0.0, &orange),
        part(Vec3::new(1.9, 0.05, 0.5), Vec3::new(0.0, -0.18, -2.1), 0.0, &dark),
        // Rear wing on two posts.
        part(Vec3::new(1.8, 0.06, 0.45), Vec3::new(0.0, 0.58, 2.0), 0.0, &dark),
        part(Vec3::new(0.08, 0.3, 0.1), Vec3::new(-0.6, 0.4, 2.0), 0.0, &dark),
        part(Vec3::new(0.08, 0.3, 0.1), Vec3::new(0.6, 0.4, 2.0), 0.0, &dark),
        // Lamps.
        part(Vec3::new(0.4, 0.08, 0.12), Vec3::new(-0.6, 0.18, -2.24), 0.0, &lamp),
        part(Vec3::new(0.4, 0.08, 0.12), Vec3::new(0.6, 0.18, -2.24), 0.0, &lamp),
        part(Vec3::new(1.5, 0.08, 0.08), Vec3::new(0.0, 0.2, 2.26), 0.0, &tail),
    ];

    let wheel_mesh = meshes.add(Cylinder::new(WHEEL_R, 0.3));
    commands
        .spawn((
            Car::new(),
            Transform::from_translation(wow_to_bevy(SPAWN_WOW)),
            Visibility::default(),
        ))
        .with_children(|car| {
            for bundle in parts {
                car.spawn(bundle);
            }
            for (sx, sz) in WHEELS {
                car.spawn((
                    Wheel { front: sz < 0.0 },
                    Mesh3d(wheel_mesh.clone()),
                    MeshMaterial3d(tyre.clone()),
                    Transform::from_xyz(sx * WHEEL_X, -(RIDE - WHEEL_R), sz * WHEEL_Z),
                ));
            }
        });
}

/// Wheel positions as (x, z) signs: front left, front right, rear left, rear right.
const WHEELS: [(f32, f32); 4] = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)];

/// Rolls and steers the wheel meshes from the car's state.
pub fn animate_wheels(car: Query<&Car>, mut wheels: Query<(&Wheel, &mut Transform)>) {
    let Ok(car) = car.single() else { return };
    for (wheel, mut tf) in &mut wheels {
        let steer = if wheel.front { -car.steer } else { 0.0 };
        // The cylinder's axis is Y; lay it along X, roll it, then steer it.
        tf.rotation = Quat::from_rotation_y(steer)
            * Quat::from_rotation_x(car.spin)
            * Quat::from_rotation_z(FRAC_PI_2);
    }
}

/// Reads the keys, advances the car, streams the world around it and writes its `Transform`.
/// WASD or arrows drive, Space is the handbrake, Shift boosts, R puts the car back at the
/// Crossroads.
pub fn drive(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    world: WorldCollision,
    mut car: Query<(&mut Car, &mut Transform)>,
    mut focus: ResMut<ViewFocus>,
    mut viewer: ResMut<Viewer>,
) {
    let Ok((mut car, mut tf)) = car.single_mut() else {
        return;
    };
    let dt = time.delta_secs().min(1.0 / 20.0);
    if dt <= 0.0 {
        return;
    }
    if keys.just_pressed(KeyCode::KeyR) {
        *car = Car::new();
    }

    if !car.placed {
        // Hold in the air and stream the tiles under the spawn, unpaced so they load without
        // stalling the frame; the first ray to hit ground drops the car onto it.
        *focus = ViewFocus::body(bevy_to_wow(car.pos), false);
        let origin = car.pos;
        if let Some(hit) = world.ray_body(origin, Dir3::NEG_Y, 1500.0) {
            car.pos.y = origin.y - hit.distance + RIDE + 1.0;
            car.vel = Vec3::ZERO;
            car.up = Vec3::Y;
            car.placed = true;
        }
        tf.translation = car.pos;
        tf.rotation = car.rotation();
        viewer.at = None;
        return;
    }

    let input = Input::read(&keys);
    let sub_dt = dt / SUBSTEPS as f32;
    for _ in 0..SUBSTEPS {
        step(&mut car, &input, sub_dt, &world);
    }
    // Driven off the end of what has streamed: wait in the air for the ground to arrive.
    if car.lost > 2.0 {
        car.placed = false;
        car.pos.y += 300.0;
        car.vel = Vec3::ZERO;
    }

    *focus = ViewFocus::body(bevy_to_wow(car.pos), true);
    viewer.at = Some(car.pos);
    viewer.height = 1.5;
    viewer.planar_speed = Vec3::new(car.vel.x, 0.0, car.vel.z).length();
    tf.translation = car.pos;
    tf.rotation = car.rotation();
}

/// One physics step.
fn step(car: &mut Car, input: &Input, dt: f32, world: &WorldCollision) {
    // The wheel rays, straight down from a yard over each wheel centre.
    let rot = car.rotation();
    let mut contacts = [None::<Vec3>; 4];
    let mut touching = 0;
    for (i, (sx, sz)) in WHEELS.iter().enumerate() {
        let mount = car.pos + rot * Vec3::new(sx * WHEEL_X, -(RIDE - WHEEL_R), sz * WHEEL_Z);
        let origin = mount + Vec3::Y;
        if let Some(hit) = world.ray_body(origin, Dir3::NEG_Y, 2.2) {
            contacts[i] = Some(origin - Vec3::Y * hit.distance);
            if hit.distance <= 1.0 + WHEEL_R + 0.15 {
                touching += 1;
            }
        }
    }
    let found = contacts.iter().flatten().count();
    car.lost = if found == 0 { car.lost + dt } else { 0.0 };
    let grounded = touching >= 2;

    // The ground plane through the four contacts; in the air the body eases level.
    if let [Some(fl), Some(fr), Some(rl), Some(rr)] = contacts {
        let forward = (fl + fr) * 0.5 - (rl + rr) * 0.5;
        let right = (fr + rr) * 0.5 - (fl + rl) * 0.5;
        let normal = right.cross(forward).normalize_or_zero();
        if normal.y > 0.3 {
            car.up = car.up.lerp(normal, smooth(dt, 10.0)).normalize_or(Vec3::Y);
        }
    } else if !grounded {
        car.up = car.up.lerp(Vec3::Y, smooth(dt, 1.5)).normalize_or(Vec3::Y);
    }

    let speed = car.speed();
    let max_steer = (0.55 / (1.0 + speed / 20.0)).max(0.12);
    car.steer += (input.steer * max_steer - car.steer) * smooth(dt, 8.0);

    let (mut fwd, _) = car.basis();
    let v_f = car.vel.dot(fwd);
    if grounded {
        // Bicycle model: the heading turns with the front wheels; right is negative yaw.
        let mut rate = -(v_f / WHEELBASE) * car.steer.tan();
        if input.handbrake {
            rate *= 1.5;
        }
        car.yaw += rate.clamp(-2.2, 2.2) * dt;
    }
    let (new_fwd, right) = car.basis();
    fwd = new_fwd;

    if grounded {
        let mut v_f = car.vel.dot(fwd);
        let mut v_r = car.vel.dot(right);
        let v_u = car.vel.dot(car.up);

        let boost = if input.boost { BOOST } else { 1.0 };
        let top = TOP_SPEED * if input.boost { 1.15 } else { 1.0 };
        let mut a = 0.0;
        if input.throttle > 0.0 {
            a = if v_f < -1.0 {
                BRAKE_DECEL
            } else {
                ENGINE_ACCEL * boost * (1.0 - v_f / top).clamp(0.0, 1.0)
            };
        } else if input.throttle < 0.0 {
            a = if v_f > 1.0 {
                -BRAKE_DECEL
            } else {
                -REVERSE_ACCEL * (1.0 - (-v_f / REVERSE_MAX)).clamp(0.0, 1.0)
            };
        }
        // Rolling and air drag.
        a -= v_f * 0.15 + v_f * v_f.abs() * 0.0004;
        v_f += a * dt;
        if input.throttle == 0.0 && v_f.abs() < 0.5 {
            v_f = 0.0;
        }
        if input.handbrake {
            v_f *= (-0.35 * dt).exp();
        }
        v_r *= (-(if input.handbrake { GRIP_HANDBRAKE } else { GRIP }) * dt).exp();
        car.vel = fwd * v_f + right * v_r + car.up * v_u;
    }

    car.vel.y -= GRAVITY * dt;

    if grounded {
        let points: Vec<Vec3> = contacts.iter().flatten().copied().collect();
        let avg = points.iter().sum::<Vec3>() / points.len() as f32;
        let h = (car.pos - avg).dot(car.up);
        // Pushes only while compressed; the rest height includes the sag under gravity.
        let compress = RIDE + GRAVITY / SPRING_K - h;
        if compress > 0.0 {
            let a = (SPRING_K * compress - SPRING_C * car.vel.dot(car.up)).max(0.0);
            car.vel += car.up * a * dt;
        }
        if h < MIN_HEIGHT {
            car.pos += car.up * (MIN_HEIGHT - h);
            let v_u = car.vel.dot(car.up);
            if v_u < 0.0 {
                car.vel -= car.up * v_u;
            }
        }
    }

    car.pos += car.vel * dt;
    car.spin -= car.vel.dot(fwd) / WHEEL_R * dt;
    hit_walls(car, dt, world);
}

/// Stops the nose against steep faces (cliffs, walls, trunks): three rays along the travel
/// direction, and a face steeper than about 57° kills the speed into it.
fn hit_walls(car: &mut Car, dt: f32, world: &WorldCollision) {
    let flat = Vec3::new(car.vel.x, 0.0, car.vel.z);
    let speed = flat.length();
    if speed < 2.0 {
        return;
    }
    let dir = flat / speed;
    let Ok(ray_dir) = Dir3::new(dir) else { return };
    let side = dir.cross(Vec3::Y);
    for offset in [-0.9, 0.0, 0.9] {
        let origin = car.pos + Vec3::Y * 0.5 + side * offset;
        let Some(hit) = world.ray_body(origin, ray_dir, NOSE + speed * dt) else {
            continue;
        };
        if hit.normal.y.abs() > 0.55 || hit.distance >= NOSE + speed * dt {
            continue;
        }
        let n = Vec3::new(hit.normal.x, 0.0, hit.normal.z).normalize_or_zero();
        let into = car.vel.dot(n);
        if into < 0.0 {
            car.vel -= n * into * 1.3;
            car.vel *= 0.85;
        }
        car.pos += n * (NOSE - hit.distance).clamp(0.0, 0.5);
    }
}
