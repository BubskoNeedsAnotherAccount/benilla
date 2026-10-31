//! Multiplayer, browser build only. The page holds one WebSocket to a relay (`server/server.mjs`,
//! `web/net-bridge.js`); this module sends the local car's state 20 times a second and shows the
//! other drivers' cars from theirs. Every car drives itself: there is no shared physics, and each
//! client is the authority for its own car, bumping it off the others as it sees them.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use benilla_world::view::WorldCamera;

use crate::car::{orientation, smooth, spawn_model, wheel_rotation, Car, CarAssets, Wheels};
use crate::model::WHEEL_RADIUS;
use crate::smoke::{puff, smoke_interval, smoke_level, TireSmokeAssets};
use crate::trial::{TimeTrialManager, VersusLobby};

#[wasm_bindgen]
extern "C" {
    /// 0 off, 1 connecting, 2 open, 3 closed and retrying, 4 refused.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaNet"], js_name = state)]
    fn net_state() -> u32;
    /// The next message the socket received, if any.
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaNet"], js_name = poll)]
    fn net_poll() -> Option<String>;
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaNet"], js_name = send)]
    fn net_send(msg: &str);
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaNet"], js_name = error)]
    fn net_error() -> String;
}

/// State messages per second.
const SEND_HZ: f32 = 20.0;
/// A car this far from where its latest state puts it is moved there, not eased (a reset, a respawn).
const SNAP_DISTANCE: f32 = 40.0;
/// Seconds a peer's prediction may run ahead of its last message.
const MAX_EXTRAPOLATE: f32 = 0.25;
/// Seconds of silence after which a peer's car is dropped.
const STALE_AFTER: f32 = 15.0;

#[derive(Deserialize)]
#[serde(tag = "t")]
enum Incoming {
    #[serde(rename = "welcome")]
    Welcome {
        #[serde(default)]
        id: Option<u32>,
        players: Vec<Peer>,
    },
    #[serde(rename = "join")]
    Join { id: u32, name: String },
    #[serde(rename = "leave")]
    Leave { id: u32 },
    #[serde(rename = "s")]
    State(State),
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Peer {
    id: u32,
    name: String,
}

/// One driver's car, as the relay forwards it (Bevy coordinates).
#[derive(Deserialize)]
struct State {
    id: u32,
    p: [f32; 3],
    v: [f32; 3],
    y: f32,
    u: [f32; 3],
    n: u8,
    /// Tyre smoke level (smoke::smoke_level); older clients send none.
    #[serde(default)]
    d: u8,
    /// The versus course the driver is on, if any.
    #[serde(default)]
    r: Option<String>,
    /// 1 while that course's start is counting down.
    #[serde(default)]
    c: u8,
}

/// A driver counting down this long ago or less is still on the grid.
const GRID_FRESH: f32 = 2.0;

/// The other drivers on the server.
#[derive(Resource, Default)]
struct Roster {
    names: HashMap<u32, String>,
    cars: HashMap<u32, Entity>,
    /// Seconds since the last state went out.
    since_send: f32,
    /// The socket was open on the previous frame, so a reopening starts from an empty roster.
    was_open: bool,
    /// This client's id, from the relay's welcome.
    my_id: Option<u32>,
}

/// Tells the race code who is online and who is counting down on the same versus course.
fn fill_lobby(
    time: Res<Time>,
    roster: Res<Roster>,
    manager: Res<TimeTrialManager>,
    mut lobby: ResMut<VersusLobby>,
    cars: Query<&RemoteCar>,
) {
    let now = time.elapsed_secs();
    lobby.online = net_state() == 2;
    lobby.my_id = roster.my_id;
    lobby.rivals.clear();
    if let Some((course, _)) = manager.versus_tag() {
        lobby.rivals.extend(cars.iter().filter_map(|car| {
            let (r, said) = car.grid.as_ref()?;
            (r == course && now - said <= GRID_FRESH).then_some(car.id)
        }));
    }
    lobby.rivals.sort_unstable();
}

/// A car driven by another player.
#[derive(Component)]
struct RemoteCar {
    id: u32,
    pos: Vec3,
    vel: Vec3,
    yaw: f32,
    up: Vec3,
    nitro: bool,
    /// `Time::elapsed_secs` of the latest state.
    seen: f32,
    placed: bool,
    wheels: Wheels,
    smoke: u8,
    /// Seconds until the next puff.
    smoke_wait: f32,
    /// Wheel roll angle, from the speed along the car's heading.
    spin: f32,
    /// The versus course this driver is counting down on, and when that was last said.
    grid: Option<(String, f32)>,
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Roster>()
            .add_systems(
                Update,
                (
                    receive,
                    fill_lobby,
                    send_state,
                    ease_remote_cars,
                    roll_remote_wheels,
                    smoke_remote_cars,
                    collide_cars,
                )
                    .chain()
                    .after(crate::car::drive),
            )
            .add_systems(EguiPrimaryContextPass, hud);
    }
}

fn drop_all(commands: &mut Commands, roster: &mut Roster) {
    for (_, e) in roster.cars.drain() {
        commands.entity(e).despawn();
    }
    roster.names.clear();
}

/// Reads what the socket has received: who is here, and where their cars are.
fn receive(
    mut commands: Commands,
    time: Res<Time>,
    assets: Res<CarAssets>,
    mut roster: ResMut<Roster>,
    mut cars: Query<&mut RemoteCar>,
) {
    let open = net_state() == 2;
    if roster.was_open && !open {
        drop_all(&mut commands, &mut roster);
    }
    roster.was_open = open;

    // A frame's worth and no more: the page caps the inbox, and a backlog is not worth catching up on.
    for _ in 0..256 {
        let Some(raw) = net_poll() else { break };
        let Ok(msg) = serde_json::from_str::<Incoming>(&raw) else {
            continue;
        };
        match msg {
            Incoming::Welcome { id, players } => {
                drop_all(&mut commands, &mut roster);
                roster.names = players.into_iter().map(|p| (p.id, p.name)).collect();
                roster.my_id = id;
            }
            Incoming::Join { id, name } => {
                roster.names.insert(id, name);
            }
            Incoming::Leave { id } => {
                roster.names.remove(&id);
                if let Some(e) = roster.cars.remove(&id) {
                    commands.entity(e).despawn();
                }
            }
            Incoming::State(s) => {
                let (pos, vel, up) = (Vec3::from(s.p), Vec3::from(s.v), Vec3::from(s.u));
                if !(pos.is_finite() && vel.is_finite() && up.is_finite() && s.y.is_finite()) {
                    continue;
                }
                let up = up.normalize_or(Vec3::Y);
                let seen = time.elapsed_secs();
                let grid = s.r.filter(|_| s.c != 0).map(|r| (r, seen));
                if let Some(&e) = roster.cars.get(&s.id) {
                    if let Ok(mut car) = cars.get_mut(e) {
                        car.pos = pos;
                        car.vel = vel;
                        car.yaw = s.y;
                        car.up = up;
                        car.nitro = s.n != 0;
                        car.smoke = s.d.min(3);
                        car.seen = seen;
                        car.grid = grid;
                    }
                    continue;
                }
                let e = spawn_remote_car(
                    &mut commands,
                    &assets,
                    RemoteCar {
                        id: s.id,
                        pos,
                        vel,
                        yaw: s.y,
                        up,
                        nitro: s.n != 0,
                        seen,
                        placed: false,
                        wheels: Wheels {
                            back: Entity::PLACEHOLDER,
                            front: [Entity::PLACEHOLDER; 2],
                        },
                        spin: 0.0,
                        smoke: s.d.min(3),
                        smoke_wait: 0.0,
                        grid,
                    },
                );
                roster.cars.insert(s.id, e);
            }
            Incoming::Other => {}
        }
    }
}

/// A remote car looks like the local one, wheels included.
fn spawn_remote_car(commands: &mut Commands, assets: &CarAssets, mut car: RemoteCar) -> Entity {
    let at = Transform::from_translation(car.pos).with_rotation(orientation(car.yaw, car.up));
    let e = commands.spawn((at, Visibility::default())).id();
    car.wheels = spawn_model(commands, assets, e);
    commands.entity(e).insert(car);
    e
}

/// Remote cars puff at this multiple of the local car's interval.
const REMOTE_SMOKE_SLOWDOWN: f32 = 2.0;
/// A remote car this far from the camera, yards, puffs nothing: its smoke is a speck.
const REMOTE_SMOKE_RANGE: f32 = 150.0;
/// Puffs showing at once, past which remote cars add none.
const REMOTE_SMOKE_CAP: usize = 120;

/// Puffs tyre smoke from the remote cars that report drifting, braking hard, a burnout or
/// 200 km/h, thinner than the local car's and only near the camera.
fn smoke_remote_cars(
    mut commands: Commands,
    time: Res<Time>,
    smoke: Option<ResMut<TireSmokeAssets>>,
    settings: Res<crate::settings::Settings>,
    camera: Query<&GlobalTransform, With<WorldCamera>>,
    mut cars: Query<(&mut RemoteCar, &Transform)>,
) {
    let Some(mut smoke) = smoke else { return };
    if !settings.tire_smoke {
        return;
    }
    let Ok(camera) = camera.single() else { return };
    let eye = camera.translation();
    let dt = time.delta_secs();
    for (mut car, tf) in &mut cars {
        if car.smoke == 0 {
            car.smoke_wait = 0.0;
            continue;
        }
        car.smoke_wait -= dt;
        if car.smoke_wait > 0.0 {
            continue;
        }
        car.smoke_wait = smoke_interval(car.smoke) * REMOTE_SMOKE_SLOWDOWN;
        if smoke.live() >= REMOTE_SMOKE_CAP
            || tf.translation.distance_squared(eye) > REMOTE_SMOKE_RANGE * REMOTE_SMOKE_RANGE
        {
            continue;
        }
        puff(
            &mut commands,
            &mut smoke,
            tf.translation,
            tf.rotation,
            car.vel,
            car.smoke,
        );
    }
}

/// Rolls each remote car's wheels at the speed along its heading.
fn roll_remote_wheels(
    time: Res<Time>,
    mut cars: Query<&mut RemoteCar>,
    mut transforms: Query<&mut Transform, Without<RemoteCar>>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    for mut car in &mut cars {
        let forward = Vec3::new(-car.yaw.sin(), 0.0, -car.yaw.cos());
        car.spin += car.vel.dot(forward) / WHEEL_RADIUS * dt;
        let roll = wheel_rotation(0.0, car.spin);
        for wheel in [car.wheels.back, car.wheels.front[0], car.wheels.front[1]] {
            if let Ok(mut tf) = transforms.get_mut(wheel) {
                tf.rotation = roll;
            }
        }
    }
}
/// Sends the local car's state, if the socket is open and the car has found the ground.
fn send_state(
    time: Res<Time>,
    mut roster: ResMut<Roster>,
    manager: Res<TimeTrialManager>,
    car: Query<&Car>,
) {
    roster.since_send += time.delta_secs();
    if roster.since_send < 1.0 / SEND_HZ || net_state() != 2 {
        return;
    }
    let Ok(car) = car.single() else { return };
    if !car.placed {
        return;
    }
    roster.since_send = 0.0;
    let grid = manager.versus_tag();
    let msg = serde_json::json!({
        "r": grid.map(|(course, _)| course),
        "c": u8::from(grid.is_some_and(|(_, counting)| counting)),
        "t": "s",
        "p": [car.pos.x, car.pos.y, car.pos.z],
        "v": [car.vel.x, car.vel.y, car.vel.z],
        "y": car.yaw,
        "u": [car.up.x, car.up.y, car.up.z],
        "n": u8::from(car.nitro_timer > 0.0),
        "d": smoke_level(car),
    });
    net_send(&msg.to_string());
}

/// Moves each remote car toward where its latest state says it is now, so 20 updates a second look smooth.
fn ease_remote_cars(
    mut commands: Commands,
    time: Res<Time>,
    mut roster: ResMut<Roster>,
    mut cars: Query<(Entity, &mut RemoteCar, &mut Transform)>,
) {
    let dt = time.delta_secs().min(1.0 / 20.0);
    let now = time.elapsed_secs();
    for (e, mut car, mut tf) in &mut cars {
        if now - car.seen > STALE_AFTER {
            roster.cars.remove(&car.id);
            commands.entity(e).despawn();
            continue;
        }
        let ahead = (now - car.seen).clamp(0.0, MAX_EXTRAPOLATE);
        let want = car.pos + car.vel * ahead;
        if !car.placed || tf.translation.distance(want) > SNAP_DISTANCE {
            tf.translation = want;
            tf.rotation = orientation(car.yaw, car.up);
            car.placed = true;
            continue;
        }
        tf.translation = tf.translation.lerp(want, smooth(dt, 14.0));
        tf.rotation = tf
            .rotation
            .slerp(orientation(car.yaw, car.up), smooth(dt, 14.0));
    }
}

/// Each car is three circles along its length, flat on the ground.
const BODY_RADIUS: f32 = 1.15;
const BODY_SPOTS: [f32; 3] = [-1.2, 0.0, 1.2];
/// Cars on different levels (a bridge over a road) do not touch.
const HEIGHT_REACH: f32 = 1.8;
/// Share of the closing speed kept as bounce.
const BOUNCE: f32 = 0.3;
/// Most the local car is pushed out of another in one frame, yards.
const MAX_PUSH: f32 = 2.0;

/// Cars bump. Every client runs this for its own car against the others' as they appear to it:
/// each pushes itself half the overlap away and takes half the impulse, so the two sides agree
/// without any shared physics.
fn collide_cars(
    mut local: Query<(&mut Car, &mut Transform), Without<RemoteCar>>,
    remotes: Query<(&RemoteCar, &Transform), Without<Car>>,
) {
    let Ok((mut car, mut tf)) = local.single_mut() else {
        return;
    };
    if !car.placed {
        return;
    }
    let heading = |yaw: f32| Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let mine = heading(car.yaw);
    for (other, otf) in &remotes {
        if !other.placed || (otf.translation.y - car.pos.y).abs() > HEIGHT_REACH {
            continue;
        }
        let theirs = heading(other.yaw);
        // The deepest overlap among the circle pairs.
        let mut hit: Option<(Vec3, f32)> = None;
        for a in BODY_SPOTS {
            for b in BODY_SPOTS {
                let delta = (car.pos + mine * a) - (otf.translation + theirs * b);
                let delta = Vec3::new(delta.x, 0.0, delta.z);
                let dist = delta.length();
                let depth = 2.0 * BODY_RADIUS - dist;
                if depth > 0.0 && hit.is_none_or(|(_, d)| depth > d) {
                    hit = Some((delta.normalize_or(-mine), depth));
                }
            }
        }
        let Some((normal, depth)) = hit else { continue };
        let push = (depth * 0.5).min(MAX_PUSH);
        car.pos += normal * push;
        let closing = (car.vel - other.vel).dot(normal);
        if closing < 0.0 {
            car.vel -= normal * closing * (1.0 + BOUNCE) * 0.5;
        }
        tf.translation = car.pos;
    }
}

const TAG_FONT: egui::FontId = egui::FontId::proportional(15.0);

/// Name tags over the other cars, and a line saying how many are driving.
fn hud(
    mut contexts: EguiContexts,
    roster: Res<Roster>,
    cars: Query<(&RemoteCar, &GlobalTransform)>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    mut warmed: Local<bool>,
) {
    let Ok(ctx) = contexts.ctx_mut() else { return };
    // The tags' glyphs go into the font atlas now, not on the frame a driver's tag first needs one
    // (the atlas growing is a re-upload).
    if !*warmed {
        *warmed = true;
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("net_tags"),
        ));
        let glyphs: String = (' '..='~')
            .chain('\u{a0}'..='\u{ff}')
            .chain(['⚡', '…'])
            .collect();
        painter.layout_no_wrap(glyphs, TAG_FONT, egui::Color32::WHITE);
    }
    let state = net_state();
    if state == 0 {
        return;
    }

    let line = match state {
        1 => "connecting…".to_string(),
        2 => format!("online · {} driving", roster.names.len() + 1),
        4 => format!("not connected: {}", net_error()),
        _ => format!("reconnecting… {}", net_error()),
    };
    egui::Area::new(egui::Id::new("net_status"))
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(line)
                    .color(egui::Color32::WHITE)
                    .background_color(egui::Color32::from_black_alpha(150)),
            );
        });

    let Ok((camera, camera_tf)) = camera.single() else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("net_tags"),
    ));
    for (car, gt) in &cars {
        let tag_at = gt.translation() + Vec3::Y * 2.2;
        // Behind the camera, or too far to read.
        let dist = camera_tf.translation().distance(tag_at);
        if dist > 400.0
            || (tag_at - camera_tf.translation()).dot(camera_tf.forward().as_vec3()) <= 0.0
        {
            continue;
        }
        let Ok(p) = camera.world_to_viewport(camera_tf, tag_at) else {
            continue;
        };
        let name = roster
            .names
            .get(&car.id)
            .map(String::as_str)
            .unwrap_or("driver");
        let text = if car.nitro {
            format!("{name} ⚡")
        } else {
            name.to_string()
        };
        let font = TAG_FONT;
        let pos = egui::pos2(p.x, p.y);
        let galley = painter.layout_no_wrap(text, font, egui::Color32::WHITE);
        let rect = egui::Align2::CENTER_BOTTOM.anchor_size(pos, galley.size());
        painter.rect_filled(
            rect.expand2(egui::vec2(6.0, 2.0)),
            4.0,
            egui::Color32::from_black_alpha(160),
        );
        painter.galley(rect.min, galley, egui::Color32::WHITE);
    }
}
