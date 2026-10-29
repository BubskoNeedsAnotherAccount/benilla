//! The car: a procedural low-poly Lamborghini and its arcade physics.
//!
//! The engine runs no dynamics (`WorldPlugins` disables avian's solver and force plugins), so the
//! car integrates itself: four wheel rays against the streamed terrain and building colliders fit
//! a ground plane, a spring holds the body over it, and a bicycle model steers it. All lengths are
//! yards, as in the world.

use std::f32::consts::PI;

use bevy::prelude::*;

use crate::model::{self, Finish, PartKind};

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::collision::WorldCollision;
use benilla_world::terrain_stream::ViewFocus;
use benilla_world::view::Viewer;

/// Lakeshire, Redridge Mountains in Eastern Kingdoms (default spawn).
pub const LAKESHIRE_WOW: [f32; 3] = [-9429.1, -2231.4, 110.0];
pub const LAKESHIRE_YAW_DEG: f32 = -90.0;

/// The Crossroads, The Barrens in Kalimdor.
pub const CROSSROADS_WOW: [f32; 3] = [-437.0, -2596.0, 400.0];
pub const CROSSROADS_YAW_DEG: f32 = 0.0;

/// Goldshire, Elwynn Forest in Eastern Kingdoms.
pub const GOLDSHIRE_WOW: [f32; 3] = [-9465.0, 60.0, 110.0];
pub const GOLDSHIRE_YAW_DEG: f32 = 0.0;

/// Default spawn point in WoW coordinates: Lakeshire in Redridge Mountains.
pub const SPAWN_WOW: [f32; 3] = LAKESHIRE_WOW;
pub const SPAWN_YAW_DEG: f32 = LAKESHIRE_YAW_DEG;

/// Half the track and wheelbase, to the wheel centres.
pub const WHEEL_X: f32 = 0.88;
pub const WHEEL_Z: f32 = 1.38;
pub const WHEEL_R: f32 = 0.34;
/// Body centre height above the ground when the wheels rest on it.
pub const RIDE: f32 = 0.48;
/// Front axle to rear axle.
const WHEELBASE: f32 = 2.76;

/// Spring stiffness and damping of the body over the ground plane.
const SPRING_K: f32 = 180.0;
const SPRING_C: f32 = 24.0;
/// The WoW gravity the world's physics is set to (`world_plugins`), in yd/s².
const GRAVITY: f32 = 23.291_105;
/// Lowest the body centre may sink toward the ground plane.
const MIN_HEIGHT: f32 = 0.44;

const ENGINE_ACCEL: f32 = 35.0;
const BRAKE_DECEL: f32 = 65.0;
const REVERSE_ACCEL: f32 = 18.0;
const REVERSE_MAX: f32 = 33.0;
/// Top speed in yd/s. nitro boosts it even further. (Scaled 1.5x to match 1.5x model scale)
pub const TOP_SPEED: f32 = 175.0;
const GRIP: f32 = 7.5;
const GRIP_HANDBRAKE: f32 = 1.3;

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
    pub placed: bool,
    /// Seconds with no wheel ray finding anything, as when driving ahead of the streamer.
    lost: f32,
    /// Cooldown until nitro can be triggered again (seconds).
    pub nitro_cooldown: f32,
    /// Active nitro boost timer (seconds remaining in 1s burst).
    pub nitro_timer: f32,
    /// Flag set for one frame when nitro is engaged (triggers sound).
    pub just_used_nitro: bool,
    /// Whether wheels are currently touching terrain.
    pub grounded: bool,
    /// Driver's current throttle input (0.0 to 1.0).
    pub throttle_input: f32,
    /// Driver's current brake input (0.0 to 1.0).
    pub brake_input: f32,
    /// Driver's handbrake state.
    pub handbrake_input: bool,
    /// Absolute lateral speed in yards/sec (for skid detection).
    pub lateral_speed: f32,
    /// Whether the car is currently in burnout mode (throttle + brake held while stationary).
    pub burnout: bool,
    /// Spawn/reset position in Bevy world coordinates.
    pub spawn_pos: Vec3,
    /// Spawn/reset yaw in radians.
    pub spawn_yaw: f32,
}

impl Car {
    pub fn new() -> Self {
        let is_kalimdor = std::env::var("WOW_MAP").map(|v| v == "1").unwrap_or(false);
        let (spawn_wow, spawn_yaw_deg) = if is_kalimdor {
            (CROSSROADS_WOW, CROSSROADS_YAW_DEG)
        } else {
            (LAKESHIRE_WOW, LAKESHIRE_YAW_DEG)
        };
        let spawn_pos = wow_to_bevy(spawn_wow);
        let spawn_yaw = spawn_yaw_deg.to_radians();

        Self {
            pos: spawn_pos,
            vel: Vec3::ZERO,
            yaw: spawn_yaw,
            up: Vec3::Y,
            steer: 0.0,
            spin: 0.0,
            placed: false,
            lost: 0.0,
            nitro_cooldown: 0.0,
            nitro_timer: 0.0,
            just_used_nitro: false,
            grounded: false,
            throttle_input: 0.0,
            brake_input: 0.0,
            handbrake_input: false,
            lateral_speed: 0.0,
            burnout: false,
            spawn_pos,
            spawn_yaw,
        }
    }

    /// Resets all motion and places the car back at its current spawn point.
    pub fn reset_to_spawn(&mut self) {
        self.pos = self.spawn_pos;
        self.vel = Vec3::ZERO;
        self.yaw = self.spawn_yaw;
        self.up = Vec3::Y;
        self.steer = 0.0;
        self.spin = 0.0;
        self.placed = false;
        self.lost = 0.0;
        self.nitro_cooldown = 0.0;
        self.nitro_timer = 0.0;
        self.just_used_nitro = false;
        self.grounded = false;
        self.throttle_input = 0.0;
        self.brake_input = 0.0;
        self.handbrake_input = false;
        self.lateral_speed = 0.0;
        self.burnout = false;
    }

    /// Sets a new spawn location and resets the car to it.
    pub fn set_spawn(&mut self, wow_coords: [f32; 3], yaw_rad: f32) {
        self.spawn_pos = wow_to_bevy(wow_coords);
        self.spawn_yaw = yaw_rad;
        self.reset_to_spawn();
    }

    /// The speed over the ground in yd/s.
    pub fn speed(&self) -> f32 {
        self.vel.length()
    }

    /// The heading on the ground plane: forward and right.
    pub fn basis(&self) -> (Vec3, Vec3) {
        basis(self.yaw, self.up)
    }

    pub fn rotation(&self) -> Quat {
        orientation(self.yaw, self.up)
    }
}

/// The body's rotation for a heading on the plane with normal `up`.
pub fn orientation(yaw: f32, up: Vec3) -> Quat {
    let (f, r) = basis(yaw, up);
    Quat::from_mat3(&Mat3::from_cols(r, up, -f))
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
    brake: f32,
    steer: f32,
    handbrake: bool,
}

impl Input {
    fn read(keys: &ButtonInput<KeyCode>) -> Self {
        let pressed = |keys_slice: &[KeyCode]| keys_slice.iter().any(|k| keys.pressed(*k));
        let throttle = if pressed(&[KeyCode::KeyW, KeyCode::ArrowUp]) {
            1.0
        } else {
            0.0
        };
        let brake = if pressed(&[KeyCode::KeyS, KeyCode::ArrowDown]) {
            1.0
        } else {
            0.0
        };
        let steer = (if pressed(&[KeyCode::KeyD, KeyCode::ArrowRight]) {
            1.0
        } else {
            0.0
        }) - (if pressed(&[KeyCode::KeyA, KeyCode::ArrowLeft]) {
            1.0
        } else {
            0.0
        });
        Self {
            throttle,
            brake,
            steer,
            handbrake: keys.pressed(KeyCode::Space),
        }
    }
}

/// The car's meshes and materials, built once and shared by the local car and every remote one.
#[derive(Resource)]
pub struct CarAssets {
    parts: Vec<(PartKind, Vec3, Handle<Mesh>, Handle<StandardMaterial>)>,
}

/// The wheel entities of a spawned model: back pair, front left, front right.
#[derive(Clone, Copy)]
pub struct Wheels {
    pub back: Entity,
    pub front: [Entity; 2],
}

/// Builds the car's parts from the embedded glb. Unlit and double-sided, as the engine's lighting
/// does not reach glTF materials.
pub fn load_car_assets(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let (parts, texture) = model::load();
    let texture = images.add(texture);
    let base = StandardMaterial {
        unlit: true,
        cull_mode: None,
        ..default()
    };
    let solid = materials.add(base.clone());
    let textured = materials.add(StandardMaterial {
        base_color_texture: Some(texture),
        ..base.clone()
    });
    let glass = materials.add(StandardMaterial {
        alpha_mode: AlphaMode::Blend,
        ..base
    });
    let parts = parts
        .into_iter()
        .map(|p| {
            let material = match p.finish {
                Finish::Plain => solid.clone(),
                Finish::Textured => textured.clone(),
                Finish::Glass => glass.clone(),
            };
            (p.kind, p.pivot, meshes.add(p.mesh), material)
        })
        .collect();
    commands.insert_resource(CarAssets { parts });
}

/// Spawns the car's model as a child of `parent`: the body and four wheels, which `Wheels` names.
/// The model faces +Z; the car faces -Z, so it is turned half way round and set on the wheels' ground.
pub fn spawn_model(commands: &mut Commands, assets: &CarAssets, parent: Entity) -> Wheels {
    let root = commands
        .spawn((
            Transform::from_xyz(0.0, -RIDE, model::AXLE_MID * model::CAR_SCALE)
                .with_rotation(Quat::from_rotation_y(PI))
                .with_scale(Vec3::splat(model::CAR_SCALE)),
            Visibility::default(),
            ChildOf(parent),
        ))
        .id();
    let mut wheels = Wheels {
        back: Entity::PLACEHOLDER,
        front: [Entity::PLACEHOLDER; 2],
    };
    for (kind, pivot, mesh, material) in &assets.parts {
        let id = commands
            .spawn((
                Mesh3d(mesh.clone()),
                MeshMaterial3d(material.clone()),
                Transform::from_translation(*pivot),
                ChildOf(root),
            ))
            .id();
        let slot = match kind {
            PartKind::Body => continue,
            PartKind::BackWheels => &mut wheels.back,
            PartKind::FrontLeft => &mut wheels.front[0],
            PartKind::FrontRight => &mut wheels.front[1],
        };
        // A wheel's parts share its pivot; the first one spawned carries the others.
        if *slot == Entity::PLACEHOLDER {
            *slot = id;
        } else {
            commands
                .entity(id)
                .insert((Transform::default(), ChildOf(*slot)));
        }
    }
    wheels
}

/// Spawns the player's car.
pub fn spawn_car(mut commands: Commands, assets: Res<CarAssets>) {
    let car = commands
        .spawn((
            Car::new(),
            Transform::from_translation(wow_to_bevy(SPAWN_WOW)),
            Visibility::default(),
        ))
        .id();
    let wheels = spawn_model(&mut commands, &assets, car);
    commands.entity(wheels.back).insert(Wheel { front: false });
    for w in wheels.front {
        commands.entity(w).insert(Wheel { front: true });
    }
}

/// Updates newly spawned glTF materials to be unlit and double-sided so they render under the
/// engine's custom lighting environment without depending on Bevy's cluster lighting.
pub fn prepare_car_materials(
    mut materials: ResMut<Assets<StandardMaterial>>,
    query: Query<&MeshMaterial3d<StandardMaterial>, Added<MeshMaterial3d<StandardMaterial>>>,
    mut count: Local<usize>,
) {
    let mut updated = 0;
    for handle in &query {
        if let Some(mat) = materials.get_mut(handle) {
            if !mat.unlit || mat.cull_mode.is_some() {
                mat.unlit = true;
                mat.cull_mode = None;
                updated += 1;
            }
        }
    }
    if updated > 0 {
        *count += updated;
        eprintln!(
            "benilla-lambo: activated {updated} car materials (unlit, total: {})",
            *count
        );
    }
}

/// Wheel positions as (x, z) signs: front left, front right, rear left, rear right.
const WHEELS: [(f32, f32); 4] = [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)];

/// A wheel in the model's space (+X left, +Z front): steered about Y, then rolled about its axle X.
pub fn wheel_rotation(steer: f32, roll: f32) -> Quat {
    Quat::from_rotation_y(steer) * Quat::from_rotation_x(roll)
}

/// Rolls and steers the wheel meshes from the car's state.
pub fn animate_wheels(car: Query<&Car>, mut wheels: Query<(&Wheel, &mut Transform)>) {
    let Ok(car) = car.single() else { return };
    for (wheel, mut tf) in &mut wheels {
        let steer = if wheel.front { -car.steer } else { 0.0 };
        tf.rotation = wheel_rotation(steer, -car.spin);
    }
}

/// Reads the keys, advances the car, streams the world around it and writes its `Transform`.
/// WASD or arrows drive, Space is the handbrake, Shift boosts, R puts the car back at the
/// Crossroads.
pub fn drive(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    world: WorldCollision,
    manager: Option<Res<crate::trial::TimeTrialManager>>,
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
    let menu_open = manager.as_ref().map(|m| m.menu_open).unwrap_or(false);
    let in_trial = manager
        .as_ref()
        .map(|m| m.current_trial_idx.is_some())
        .unwrap_or(false);

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

    car.just_used_nitro = false;
    car.nitro_cooldown = (car.nitro_cooldown - dt).max(0.0);
    car.nitro_timer = (car.nitro_timer - dt).max(0.0);

    if in_trial {
        car.nitro_timer = 0.0;
    }

    let nitro_pressed = !menu_open
        && !in_trial
        && (keys.just_pressed(KeyCode::ShiftLeft) || keys.just_pressed(KeyCode::ShiftRight));
    if nitro_pressed && car.nitro_cooldown <= 0.0 && car.placed {
        car.nitro_cooldown = 10.0;
        car.nitro_timer = 1.0;
        car.just_used_nitro = true;
        let (fwd, _) = car.basis();
        let v_f = car.vel.dot(fwd);
        if v_f < 0.0 {
            car.vel -= fwd * v_f;
        }
        car.vel += fwd * 27.5;
    }

    let input = if menu_open {
        Input {
            throttle: 0.0,
            brake: 0.0,
            steer: 0.0,
            handbrake: true,
        }
    } else {
        Input::read(&keys)
    };
    let sub_dt = dt / SUBSTEPS as f32;
    for _ in 0..SUBSTEPS {
        step(&mut car, &input, sub_dt, &world);
    }
    // Driven off the edge of the world into the void: wait in the air for ground to arrive.
    if car.lost > 6.0 {
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
    let down_dir = Dir3::new(-car.up).unwrap_or(Dir3::NEG_Y);
    for (i, (sx, sz)) in WHEELS.iter().enumerate() {
        let mount = car.pos + rot * Vec3::new(sx * WHEEL_X, -(RIDE - WHEEL_R), sz * WHEEL_Z);
        let origin = mount + car.up * 1.0;
        if let Some(hit) = world.ray_body(origin, down_dir, 2.6) {
            contacts[i] = Some(origin + down_dir.as_vec3() * hit.distance);
            if hit.distance <= 1.0 + WHEEL_R + 0.25 {
                touching += 1;
            }
        }
    }
    let found = contacts.iter().flatten().count();
    // Only accumulate `lost` if there is NO terrain at all beneath the car within 2000 yards.
    // Jumping high in the air above valid ground will not reset the car.
    let ground_below = found > 0
        || world
            .ray_body(car.pos + Vec3::Y * 2.0, Dir3::NEG_Y, 2000.0)
            .is_some();
    car.lost = if !ground_below { car.lost + dt } else { 0.0 };
    let grounded = touching >= 2;

    // Ground plane normal through contacts: supports 4-wheel or 3-wheel fit on ramp transitions.
    let normal = match contacts {
        [Some(fl), Some(fr), Some(rl), Some(rr)] => {
            let forward = (fl + fr) * 0.5 - (rl + rr) * 0.5;
            let right = (fr + rr) * 0.5 - (fl + rl) * 0.5;
            Some(right.cross(forward).normalize_or_zero())
        }
        [Some(fl), Some(fr), Some(rl), None] => {
            let forward = fl - rl;
            let right = fr - fl;
            Some(right.cross(forward).normalize_or_zero())
        }
        [Some(fl), Some(fr), None, Some(rr)] => {
            let forward = fr - rr;
            let right = fr - fl;
            Some(right.cross(forward).normalize_or_zero())
        }
        [Some(fl), None, Some(rl), Some(rr)] => {
            let forward = fl - rl;
            let right = rr - rl;
            Some(right.cross(forward).normalize_or_zero())
        }
        [None, Some(fr), Some(rl), Some(rr)] => {
            let forward = fr - rr;
            let right = rr - rl;
            Some(right.cross(forward).normalize_or_zero())
        }
        _ => None,
    };

    if let Some(norm) = normal {
        if norm.y > 0.05 {
            car.up = car.up.lerp(norm, smooth(dt, 12.0)).normalize_or(Vec3::Y);
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

    car.grounded = grounded;
    car.throttle_input = input.throttle;
    car.brake_input = input.brake;
    car.handbrake_input = input.handbrake;
    if !grounded {
        car.lateral_speed = 0.0;
        car.burnout = false;
    }

    if grounded {
        let mut v_f = car.vel.dot(fwd);
        let mut v_r = car.vel.dot(right);
        let v_u = car.vel.dot(car.up);
        car.lateral_speed = v_r.abs();

        let is_nitro = car.nitro_timer > 0.0;
        let top = if is_nitro {
            TOP_SPEED * 1.275
        } else {
            TOP_SPEED
        };

        if input.throttle > 0.0 && input.brake > 0.0 {
            // Simultaneous Acceleration + Braking:
            if v_f.abs() < 4.0 {
                // Burnout: front brakes clamp while rear wheels spin
                car.burnout = true;
                v_f *= (-25.0 * dt).exp();
                if v_f.abs() < 0.05 {
                    v_f = 0.0;
                }
                car.spin -= 45.0 * dt;
            } else {
                car.burnout = false;
                // Braking with throttle at speed: brake deceleration dominates
                let brake_force = BRAKE_DECEL;
                let engine_force = ENGINE_ACCEL * (1.0 - (v_f / top).clamp(0.0, 1.0));
                let net_a = engine_force - brake_force;
                v_f += net_a * dt;
                if v_f < 0.0 {
                    v_f = 0.0;
                }
            }
        } else if input.throttle > 0.0 || is_nitro {
            car.burnout = false;
            let mut a = if v_f < -1.0 {
                BRAKE_DECEL
            } else {
                let accel_mult = if is_nitro { 1.75 } else { 1.0 };
                ENGINE_ACCEL * accel_mult * (1.0 - v_f / top).clamp(0.0, 1.0)
            };
            a -= v_f * 0.15 + v_f * v_f.abs() * 0.0004;
            v_f += a * dt;
        } else if input.brake > 0.0 {
            car.burnout = false;
            let mut a = if v_f > 0.8 {
                -BRAKE_DECEL
            } else {
                -REVERSE_ACCEL * (1.0 - (-v_f / REVERSE_MAX)).clamp(0.0, 1.0)
            };
            a -= v_f * 0.15 + v_f * v_f.abs() * 0.0004;
            v_f += a * dt;
        } else {
            car.burnout = false;
            let a = -v_f * 0.25 - v_f * v_f.abs() * 0.0004;
            v_f += a * dt;
            if v_f.abs() < 0.3 {
                v_f = 0.0;
            }
        }

        if input.handbrake {
            v_f *= (-3.0 * dt).exp();
        }
        v_r *= (-(if input.handbrake {
            GRIP_HANDBRAKE
        } else {
            GRIP
        }) * dt)
            .exp();
        car.vel = fwd * v_f + right * v_r + car.up * v_u;
    }

    car.vel.y -= GRAVITY * dt;

    if grounded {
        // High-speed downforce keeps wheels pressed against steep ramps and half-pipes.
        let speed = car.speed();
        if speed > 8.0 {
            let downforce = (speed / TOP_SPEED).clamp(0.0, 1.4) * 16.0;
            car.vel -= car.up * downforce * dt;
        }

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
    if !car.burnout {
        car.spin -= car.vel.dot(fwd) / model::WHEEL_RADIUS * dt;
    }
    hit_walls(car, dt, world);
}

/// Stops the nose against steep faces (cliffs, walls, trunks), but allows riding up steep ramps
/// and half pipes if the car has enough speed.
fn hit_walls(car: &mut Car, dt: f32, world: &WorldCollision) {
    let speed = car.speed();
    if speed < 2.0 {
        return;
    }
    let flat = Vec3::new(car.vel.x, 0.0, car.vel.z);
    let flat_speed = flat.length();
    let dir = if flat_speed > 1.0 {
        flat / flat_speed
    } else {
        let (fwd, _) = car.basis();
        Vec3::new(fwd.x, 0.0, fwd.z).normalize_or(fwd)
    };
    let Ok(ray_dir) = Dir3::new(dir) else { return };
    let side = dir.cross(Vec3::Y);

    for offset in [-0.9, 0.0, 0.9] {
        let origin = car.pos + Vec3::Y * 0.45 + side * offset;
        let Some(hit) = world.ray_body(origin, ray_dir, NOSE + speed * dt) else {
            continue;
        };
        if hit.distance >= NOSE + speed * dt {
            continue;
        }

        // Steeper ramp threshold scaling with speed:
        // At low speed (<= 5 yd/s): normal.y >= 0.55 (up to ~57° slope).
        // At high speed (>= 35 yd/s): normal.y >= 0.08 (up to ~85° slope, like a half pipe).
        let speed_ratio = (speed / 35.0).clamp(0.0, 1.0);
        let min_ramp_normal_y = 0.55 - speed_ratio * 0.47;

        if hit.normal.y >= min_ramp_normal_y && hit.normal.y > 0.05 {
            // Drivable ramp! Transition velocity up the slope instead of crashing
            let into = car.vel.dot(hit.normal);
            if into < 0.0 {
                car.vel -= hit.normal * into;
            }
            car.pos += hit.normal * (NOSE - hit.distance).clamp(0.0, 0.5);
            car.up = car
                .up
                .lerp(hit.normal, smooth(dt, 16.0))
                .normalize_or(Vec3::Y);
        } else if hit.normal.y.abs() <= 0.55 {
            // Impassable wall or cliff steeper than current speed can climb
            let n = Vec3::new(hit.normal.x, 0.0, hit.normal.z).normalize_or_zero();
            let into = car.vel.dot(n);
            if into < 0.0 {
                car.vel -= n * into * 1.3;
                car.vel *= 0.85;
            }
            car.pos += n * (NOSE - hit.distance).clamp(0.0, 0.5);
        }
    }
}
