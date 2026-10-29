//! Multiplayer, browser build only. The page holds one WebSocket to a relay (`server/server.mjs`,
//! `web/net-bridge.js`); this module sends the local car's state 20 times a second and shows the
//! other drivers' cars from theirs. Every car drives itself: there is no shared physics, so cars
//! pass through each other and each client is the authority for its own.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts, EguiPrimaryContextPass};
use serde::Deserialize;
use wasm_bindgen::prelude::*;

use benilla_world::view::WorldCamera;

use crate::car::{orientation, smooth, Car, LamboModelConfig};

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
    #[wasm_bindgen(js_namespace = ["globalThis", "__benillaNet"], js_name = room)]
    fn net_room() -> String;
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
    Welcome { players: Vec<Peer> },
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
}

/// The other drivers in the room.
#[derive(Resource, Default)]
struct Roster {
    names: HashMap<u32, String>,
    cars: HashMap<u32, Entity>,
    /// Seconds since the last state went out.
    since_send: f32,
    /// The socket was open on the previous frame, so a reopening starts from an empty room.
    was_open: bool,
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
}

pub struct NetPlugin;

impl Plugin for NetPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Roster>()
            .add_systems(Update, (receive, send_state, ease_remote_cars).chain())
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
    asset_server: Res<AssetServer>,
    model: Res<LamboModelConfig>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
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
            Incoming::Welcome { players } => {
                drop_all(&mut commands, &mut roster);
                roster.names = players.into_iter().map(|p| (p.id, p.name)).collect();
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
                if let Some(&e) = roster.cars.get(&s.id) {
                    if let Ok(mut car) = cars.get_mut(e) {
                        car.pos = pos;
                        car.vel = vel;
                        car.yaw = s.y;
                        car.up = up;
                        car.nitro = s.n != 0;
                        car.seen = seen;
                    }
                    continue;
                }
                let e = spawn_remote_car(
                    &mut commands,
                    &asset_server,
                    &model,
                    &mut meshes,
                    &mut materials,
                    RemoteCar {
                        id: s.id,
                        pos,
                        vel,
                        yaw: s.y,
                        up,
                        nitro: s.n != 0,
                        seen,
                        placed: false,
                    },
                );
                roster.cars.insert(s.id, e);
            }
            Incoming::Other => {}
        }
    }
}

/// A remote car looks like the local one: the same model, or the plain body without it.
fn spawn_remote_car(
    commands: &mut Commands,
    asset_server: &AssetServer,
    model: &LamboModelConfig,
    meshes: &mut Assets<Mesh>,
    materials: &mut Assets<StandardMaterial>,
    car: RemoteCar,
) -> Entity {
    let at = Transform::from_translation(car.pos).with_rotation(orientation(car.yaw, car.up));
    let mut e = commands.spawn((car, at, Visibility::default()));
    e.with_children(|c| {
        if let Some(file) = &model.model_path {
            c.spawn((
                SceneRoot(asset_server.load(format!("{file}#Scene0"))),
                Transform::from_translation(model.auto_offset)
                    .with_rotation(Quat::from_rotation_y(model.auto_yaw.to_radians()))
                    .with_scale(Vec3::splat(model.auto_scale)),
                Visibility::default(),
            ));
        } else {
            c.spawn((
                Mesh3d(meshes.add(Cuboid::new(1.95, 0.5, 4.5))),
                MeshMaterial3d(materials.add(StandardMaterial {
                    base_color: Color::srgb(0.1, 0.45, 1.0),
                    unlit: true,
                    ..default()
                })),
            ));
        }
    });
    e.id()
}

/// Sends the local car's state, if the socket is open and the car has found the ground.
fn send_state(time: Res<Time>, mut roster: ResMut<Roster>, car: Query<&Car>) {
    roster.since_send += time.delta_secs();
    if roster.since_send < 1.0 / SEND_HZ || net_state() != 2 {
        return;
    }
    let Ok(car) = car.single() else { return };
    if !car.placed {
        return;
    }
    roster.since_send = 0.0;
    let msg = serde_json::json!({
        "t": "s",
        "p": [car.pos.x, car.pos.y, car.pos.z],
        "v": [car.vel.x, car.vel.y, car.vel.z],
        "y": car.yaw,
        "u": [car.up.x, car.up.y, car.up.z],
        "n": u8::from(car.nitro_timer > 0.0),
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

/// Name tags over the other cars, and a line saying who is in the room.
fn hud(
    mut contexts: EguiContexts,
    roster: Res<Roster>,
    cars: Query<(&RemoteCar, &GlobalTransform)>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
) {
    let state = net_state();
    if state == 0 {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };

    let line = match state {
        1 => "connecting…".to_string(),
        2 => format!("room {} · {} driving", net_room(), roster.names.len() + 1),
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
        let font = egui::FontId::proportional(15.0);
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
