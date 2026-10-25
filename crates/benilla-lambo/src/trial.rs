//! Time trial gamemode and race management.
//!
//! Loads races from a JSON file (`time_trials.json`), tracks start and finish coordinates,
//! map switches across continents (e.g. Kalimdor <-> Eastern Kingdoms), renders in-world
//! 3D start/finish beacon markers, provides an ESC menu to choose a race or free driving,
//! and tracks race timers and personal bests.

use std::collections::HashMap;

use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};

use benilla_assets::coords::wow_to_bevy;
use benilla_world::world_map::CurrentMap;

use crate::camera::ChaseCam;
use crate::car::Car;

/// Goldshire, Elwynn Forest in WoW coordinates (x north, y west, z up).
pub const GOLDSHIRE_WOW: [f32; 3] = [-9465.0, 60.0, 70.0];

/// A single time trial race course definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeTrial {
    pub name: String,
    pub map_id: u32,
    pub start: [f32; 3], // WoW coordinates: [x north, y west, z up]
    pub end: [f32; 3],   // WoW coordinates: [x north, y west, z up]
    /// Optional starting rotation in degrees (if None, faces directly towards finish).
    #[serde(default)]
    pub rotation: Option<f32>,
}

/// Flexible JSON coordinate definition that accepts either `[x, y, z]` or `{"x": .., "y": .., "z": ..}`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CoordDef {
    Array([f32; 3]),
    Object { x: f32, y: f32, z: f32 },
}

impl CoordDef {
    pub fn to_array(&self) -> [f32; 3] {
        match self {
            CoordDef::Array(a) => *a,
            CoordDef::Object { x, y, z } => [*x, *y, *z],
        }
    }
}

/// Flexible JSON deserialization helper for time trial definitions with common field name aliases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeTrialDef {
    pub name: String,
    #[serde(alias = "map", alias = "mapId", alias = "map_id")]
    pub map_id: u32,
    #[serde(
        alias = "start_coords",
        alias = "start_coord",
        alias = "start_pos",
        alias = "start_position"
    )]
    pub start: CoordDef,
    #[serde(
        alias = "end_coords",
        alias = "end_coord",
        alias = "end_pos",
        alias = "end_position",
        alias = "finish"
    )]
    pub end: CoordDef,
    /// Starting rotation / heading in degrees (finetunable in JSON).
    #[serde(
        default,
        alias = "yaw",
        alias = "rotation_deg",
        alias = "heading",
        alias = "start_rotation"
    )]
    pub rotation: Option<f32>,
}

impl From<TimeTrialDef> for TimeTrial {
    fn from(def: TimeTrialDef) -> Self {
        Self {
            name: def.name,
            map_id: def.map_id,
            start: def.start.to_array(),
            end: def.end.to_array(),
            rotation: def.rotation,
        }
    }
}

/// State of the active time trial.
#[derive(Debug, Clone, PartialEq)]
pub enum TrialState {
    FreeDriving,
    WaitingToStart,
    Racing { elapsed: f32 },
    Finished { final_time: f32, is_best: bool },
}

/// Global resource managing all time trials, race progress, records, and menu state.
#[derive(Resource)]
pub struct TimeTrialManager {
    pub trials: Vec<TimeTrial>,
    pub current_trial_idx: Option<usize>,
    pub state: TrialState,
    pub best_times: HashMap<String, f32>,
    pub menu_open: bool,
    pub selected_menu_idx: usize,
}

impl Default for TimeTrialManager {
    fn default() -> Self {
        let trials = load_time_trials();
        Self {
            trials,
            current_trial_idx: None,
            state: TrialState::FreeDriving,
            best_times: HashMap::new(),
            menu_open: false,
            selected_menu_idx: 0,
        }
    }
}

impl TimeTrialManager {
    /// Returns the currently active time trial, if one is selected.
    pub fn current_trial(&self) -> Option<&TimeTrial> {
        self.current_trial_idx.and_then(|idx| self.trials.get(idx))
    }
}

/// Discovers and loads time trials from JSON files or environment variable.
pub fn load_time_trials() -> Vec<TimeTrial> {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    let mut candidate_paths = Vec::new();
    if let Ok(env_path) = std::env::var("TIME_TRIALS_FILE") {
        candidate_paths.push(std::path::PathBuf::from(env_path));
    }
    candidate_paths.push(manifest.join("assets").join("time_trials.json"));
    candidate_paths.push(manifest.join("../../assets").join("time_trials.json"));
    candidate_paths.push(std::path::PathBuf::from("assets/time_trials.json"));
    candidate_paths.push(std::path::PathBuf::from("time_trials.json"));

    for path in candidate_paths {
        if path.exists() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(defs) = serde_json::from_str::<Vec<TimeTrialDef>>(&content) {
                    let trials: Vec<TimeTrial> = defs.into_iter().map(Into::into).collect();
                    if !trials.is_empty() {
                        eprintln!(
                            "benilla-lambo: loaded {} time trials from {}",
                            trials.len(),
                            path.display()
                        );
                        return trials;
                    }
                } else if let Ok(trials) = serde_json::from_str::<Vec<TimeTrial>>(&content) {
                    if !trials.is_empty() {
                        eprintln!(
                            "benilla-lambo: loaded {} time trials from {}",
                            trials.len(),
                            path.display()
                        );
                        return trials;
                    }
                }
            }
        }
    }

    eprintln!("benilla-lambo: using default built-in time trials");
    vec![
        TimeTrial {
            name: "Crossroads to Ratchet Sprint".into(),
            map_id: 1,
            start: [-437.0, -2596.0, 96.0],
            end: [-850.0, -3650.0, 30.0],
            rotation: Some(115.0),
        },
        TimeTrial {
            name: "Lakeshire to Sentinel Hill".into(),
            map_id: 0,
            start: [-9429.1, -2231.4, 68.65],
            end: [-10628.89, 1036.68, 34.06],
            rotation: Some(-90.0),
        },
        TimeTrial {
            name: "Menethil Harbor to Thelsamar".into(),
            map_id: 0,
            start: [-3792.26, -783.29, 9.06],
            end: [-5421.91, -2930.01, 347.25],
            rotation: Some(90.0),
        },
        TimeTrial {
            name: "Caverns of Time to The Great Lift".into(),
            map_id: 1,
            start: [-8180.0, -4460.0, 8.0],
            end: [-4945.0, -1350.0, 85.0],
            rotation: Some(45.0),
        },
    ]
}

/// Component tagging the root entity for the Start marker.
#[derive(Component)]
pub struct StartMarker;

/// Component tagging the root entity for the Finish marker.
#[derive(Component)]
pub struct FinishMarker;

/// Component tagging rotating/animated visual parts on markers.
#[derive(Component)]
pub struct RotatingMarker {
    pub speed: f32,
    pub bob_speed: f32,
    pub base_y: f32,
}

/// Spawns the in-world 3D visual markers for Start and Finish lines.
pub fn setup_markers(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
) {
    // START MARKER: Bright green rings and floating crystal
    let start_ring_mesh = meshes.add(Cylinder::new(12.0, 0.4));
    let start_crystal_mesh = meshes.add(Cylinder::new(2.0, 4.0));
    let start_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(0.05, 1.0, 0.35, 0.75),
        unlit: true,
        cull_mode: None,
        ..default()
    });

    commands
        .spawn((
            StartMarker,
            Transform::from_translation(Vec3::new(0.0, -9999.0, 0.0)),
            Visibility::Hidden,
        ))
        .with_children(|parent| {
            // Base ground ring
            parent.spawn((
                Mesh3d(start_ring_mesh),
                MeshMaterial3d(start_mat.clone()),
                Transform::from_xyz(0.0, 0.2, 0.0),
            ));
            // Floating rotating indicator
            parent.spawn((
                RotatingMarker {
                    speed: 2.0,
                    bob_speed: 3.0,
                    base_y: 4.5,
                },
                Mesh3d(start_crystal_mesh),
                MeshMaterial3d(start_mat),
                Transform::from_xyz(0.0, 4.5, 0.0),
            ));
        });

    // FINISH MARKER: Checkered golden beacon beam shooting into the sky, plus large ground ring
    let finish_ring_mesh = meshes.add(Cylinder::new(14.0, 0.5));
    let finish_beam_mesh = meshes.add(Cylinder::new(3.5, 600.0));
    let finish_crystal_mesh = meshes.add(Cylinder::new(2.8, 5.0));

    let finish_ring_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.85, 0.05, 0.85),
        unlit: true,
        cull_mode: None,
        ..default()
    });
    let finish_beam_mat = materials.add(StandardMaterial {
        base_color: Color::srgba(1.0, 0.8, 0.1, 0.45),
        unlit: true,
        cull_mode: None,
        ..default()
    });

    commands
        .spawn((
            FinishMarker,
            Transform::from_translation(Vec3::new(0.0, -9999.0, 0.0)),
            Visibility::Hidden,
        ))
        .with_children(|parent| {
            // Ground ring
            parent.spawn((
                Mesh3d(finish_ring_mesh),
                MeshMaterial3d(finish_ring_mat.clone()),
                Transform::from_xyz(0.0, 0.25, 0.0),
            ));
            // High visibility vertical beacon beam
            parent.spawn((
                Mesh3d(finish_beam_mesh),
                MeshMaterial3d(finish_beam_mat),
                Transform::from_xyz(0.0, 300.0, 0.0),
            ));
            // Floating rotating top beacon
            parent.spawn((
                RotatingMarker {
                    speed: 2.5,
                    bob_speed: 2.5,
                    base_y: 6.0,
                },
                Mesh3d(finish_crystal_mesh),
                MeshMaterial3d(finish_ring_mat),
                Transform::from_xyz(0.0, 6.0, 0.0),
            ));
        });
}

/// Animates the rotating crystals and bobbing parts of in-world markers.
pub fn animate_markers(time: Res<Time>, mut query: Query<(&RotatingMarker, &mut Transform)>) {
    let t = time.elapsed_secs();
    let dt = time.delta_secs();
    for (rot, mut tf) in &mut query {
        tf.rotate_y(rot.speed * dt);
        tf.translation.y = rot.base_y + (t * rot.bob_speed).sin() * 0.8;
    }
}

/// Updates position and visibility of Start and Finish markers based on active time trial.
#[allow(clippy::type_complexity)]
pub fn update_marker_positions(
    manager: Res<TimeTrialManager>,
    mut start_q: Query<
        (&mut Transform, &mut Visibility),
        (With<StartMarker>, Without<FinishMarker>),
    >,
    mut finish_q: Query<
        (&mut Transform, &mut Visibility),
        (With<FinishMarker>, Without<StartMarker>),
    >,
) {
    let (Ok((mut start_tf, mut start_vis)), Ok((mut finish_tf, mut finish_vis))) =
        (start_q.single_mut(), finish_q.single_mut())
    else {
        return;
    };

    if let Some(trial) = manager.current_trial() {
        let start_pos = wow_to_bevy(trial.start);
        let end_pos = wow_to_bevy(trial.end);

        start_tf.translation = start_pos;
        finish_tf.translation = end_pos;

        *start_vis = match manager.state {
            TrialState::WaitingToStart | TrialState::Racing { .. } => Visibility::Visible,
            _ => Visibility::Hidden,
        };
        *finish_vis = Visibility::Visible;
    } else {
        *start_vis = Visibility::Hidden;
        *finish_vis = Visibility::Hidden;
    }
}

/// Handles ESC menu toggle, keyboard navigation, and restarts with 'R'.
pub fn handle_trial_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut manager: ResMut<TimeTrialManager>,
    mut car: Query<&mut Car>,
    mut chase_cam: Query<&mut ChaseCam>,
    mut current_map: ResMut<CurrentMap>,
) {
    // ESC toggles the menu
    if keys.just_pressed(KeyCode::Escape) {
        manager.menu_open = !manager.menu_open;
    }

    // When menu is open, handle keyboard navigation
    if manager.menu_open {
        let total_items = manager.trials.len() + 2; // +2 for Crossroads and Goldshire Free Driving
        if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            if manager.selected_menu_idx == 0 {
                manager.selected_menu_idx = total_items.saturating_sub(1);
            } else {
                manager.selected_menu_idx -= 1;
            }
        }
        if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            manager.selected_menu_idx = (manager.selected_menu_idx + 1) % total_items;
        }
        if keys.just_pressed(KeyCode::Enter) || keys.just_pressed(KeyCode::Space) {
            let choice = manager.selected_menu_idx;
            apply_menu_choice(
                choice,
                &mut manager,
                &mut car,
                &mut chase_cam,
                &mut current_map,
            );
            manager.menu_open = false;
        }

        // Quick numeric selection (1 = Crossroads, 2 = Goldshire, 3.. = Trials)
        for (i, key) in [
            KeyCode::Digit1,
            KeyCode::Digit2,
            KeyCode::Digit3,
            KeyCode::Digit4,
            KeyCode::Digit5,
            KeyCode::Digit6,
            KeyCode::Digit7,
            KeyCode::Digit8,
            KeyCode::Digit9,
        ]
        .iter()
        .enumerate()
        {
            if keys.just_pressed(*key) && i < total_items {
                apply_menu_choice(i, &mut manager, &mut car, &mut chase_cam, &mut current_map);
                manager.menu_open = false;
                break;
            }
        }
        return;
    }

    // 'R' key restarts current mode
    if keys.just_pressed(KeyCode::KeyR) {
        if let Some(idx) = manager.current_trial_idx {
            start_trial_by_index(
                idx,
                &mut manager,
                &mut car,
                &mut chase_cam,
                &mut current_map,
            );
        } else if current_map.0 == 0 {
            // Free driving reset to Goldshire (Eastern Kingdoms)
            if let Ok(mut car_inst) = car.single_mut() {
                car_inst.pos =
                    wow_to_bevy([GOLDSHIRE_WOW[0], GOLDSHIRE_WOW[1], GOLDSHIRE_WOW[2] + 40.0]);
                car_inst.vel = Vec3::ZERO;
                car_inst.yaw = 0.0;
                car_inst.up = Vec3::Y;
                car_inst.placed = false;
                car_inst.steer = 0.0;
                car_inst.spin = 0.0;
                car_inst.nitro_cooldown = 0.0;
                car_inst.nitro_timer = 0.0;
                if let Ok(mut cam) = chase_cam.single_mut() {
                    cam.yaw = 0.0;
                    cam.snapped = false;
                }
            }
        } else {
            // Free driving reset to Crossroads (Kalimdor)
            if let Ok(mut car_inst) = car.single_mut() {
                *car_inst = Car::new();
                if let Ok(mut cam) = chase_cam.single_mut() {
                    cam.yaw = 0.0;
                    cam.snapped = false;
                }
            }
        }
    }
}

/// Applies a menu choice by index:
/// - 0 = Free Driving: The Barrens (Crossroads)
/// - 1 = Free Driving: Elwynn Forest (Goldshire)
/// - 2..N = Time Trials
pub fn apply_menu_choice(
    choice: usize,
    manager: &mut TimeTrialManager,
    car: &mut Query<&mut Car>,
    chase_cam: &mut Query<&mut ChaseCam>,
    current_map: &mut ResMut<CurrentMap>,
) {
    if choice == 0 {
        // Free Driving: Crossroads (The Barrens, Kalimdor)
        manager.current_trial_idx = None;
        manager.state = TrialState::FreeDriving;
        if current_map.0 != 1 {
            current_map.0 = 1;
        }
        if let Ok(mut car_inst) = car.single_mut() {
            *car_inst = Car::new();
        }
        if let Ok(mut cam) = chase_cam.single_mut() {
            cam.yaw = 0.0;
            cam.snapped = false;
        }
        eprintln!("benilla-lambo: switched to Free Driving (Crossroads - The Barrens)");
    } else if choice == 1 {
        // Free Driving: Goldshire (Elwynn Forest, Eastern Kingdoms)
        manager.current_trial_idx = None;
        manager.state = TrialState::FreeDriving;
        if current_map.0 != 0 {
            current_map.0 = 0;
        }
        if let Ok(mut car_inst) = car.single_mut() {
            car_inst.pos =
                wow_to_bevy([GOLDSHIRE_WOW[0], GOLDSHIRE_WOW[1], GOLDSHIRE_WOW[2] + 40.0]);
            car_inst.vel = Vec3::ZERO;
            car_inst.yaw = 0.0;
            car_inst.up = Vec3::Y;
            car_inst.placed = false;
            car_inst.steer = 0.0;
            car_inst.spin = 0.0;
            car_inst.nitro_cooldown = 0.0;
            car_inst.nitro_timer = 0.0;
        }
        if let Ok(mut cam) = chase_cam.single_mut() {
            cam.yaw = 0.0;
            cam.snapped = false;
        }
        eprintln!("benilla-lambo: switched to Free Driving (Goldshire - Elwynn Forest)");
    } else {
        let trial_idx = choice - 2;
        start_trial_by_index(trial_idx, manager, car, chase_cam, current_map);
    }
}

/// Starts or restarts a trial given its index in `trials`.
pub fn start_trial_by_index(
    trial_idx: usize,
    manager: &mut TimeTrialManager,
    car: &mut Query<&mut Car>,
    chase_cam: &mut Query<&mut ChaseCam>,
    current_map: &mut ResMut<CurrentMap>,
) {
    let Some(trial) = manager.trials.get(trial_idx).cloned() else {
        return;
    };
    manager.current_trial_idx = Some(trial_idx);
    manager.state = TrialState::WaitingToStart;

    // Switch map if trial is on another continent
    if current_map.0 != trial.map_id {
        eprintln!(
            "benilla-lambo: switching map from {} to {} for trial '{}'",
            current_map.0, trial.map_id, trial.name
        );
        current_map.0 = trial.map_id;
    }

    // Position car at start, facing towards finish or using the user's finetuned rotation
    let start_bevy = wow_to_bevy([trial.start[0], trial.start[1], trial.start[2] + 40.0]);
    let end_bevy = wow_to_bevy(trial.end);
    let fwd_vec = end_bevy - start_bevy;
    let initial_yaw = trial
        .rotation
        .map(|deg| deg.to_radians())
        .unwrap_or_else(|| (-fwd_vec.x).atan2(-fwd_vec.z));

    if let Ok(mut car_inst) = car.single_mut() {
        car_inst.pos = start_bevy;
        car_inst.vel = Vec3::ZERO;
        car_inst.yaw = initial_yaw;
        car_inst.up = Vec3::Y;
        car_inst.placed = false; // drop onto terrain as soon as it streams
        car_inst.steer = 0.0;
        car_inst.spin = 0.0;
        car_inst.nitro_cooldown = 0.0;
        car_inst.nitro_timer = 0.0;
    }

    if let Ok(mut cam) = chase_cam.single_mut() {
        cam.yaw = initial_yaw;
        cam.snapped = false;
    }

    eprintln!(
        "benilla-lambo: started time trial '{}' on map {} (rotation: {:.1}°)",
        trial.name,
        trial.map_id,
        initial_yaw.to_degrees()
    );
}

/// Advances race timer, triggers race start on movement, and checks finish line crossing.
pub fn update_trial_progress(
    time: Res<Time>,
    mut manager: ResMut<TimeTrialManager>,
    car: Query<&Car>,
) {
    let Ok(car) = car.single() else { return };
    let dt = time.delta_secs();

    let Some(trial) = manager.current_trial().cloned() else {
        return;
    };

    match manager.state {
        TrialState::WaitingToStart => {
            // Once car has touched ground and player hits throttle or begins rolling, start timer
            if car.placed && (car.throttle_input.abs() > 0.05 || car.speed() > 1.0) {
                manager.state = TrialState::Racing { elapsed: 0.0 };
            }
        }
        TrialState::Racing { ref mut elapsed } => {
            *elapsed += dt;
            let current_time = *elapsed;

            // Check distance to finish line
            let finish_bevy = wow_to_bevy(trial.end);
            let horizontal_dist =
                Vec2::new(car.pos.x - finish_bevy.x, car.pos.z - finish_bevy.z).length();
            let vertical_dist = (car.pos.y - finish_bevy.y).abs();

            // Finish gate radius is 16.0 yards horizontally, within 45 yards vertical
            if horizontal_dist <= 16.0 && vertical_dist <= 45.0 {
                let best = manager.best_times.get(&trial.name).copied();
                let is_best = match best {
                    Some(b) => current_time < b,
                    None => true,
                };
                if is_best {
                    manager.best_times.insert(trial.name.clone(), current_time);
                }
                manager.state = TrialState::Finished {
                    final_time: current_time,
                    is_best,
                };
                eprintln!(
                    "benilla-lambo: FINISHED '{}'! Time: {:.2}s (Best: {})",
                    trial.name,
                    current_time,
                    if is_best {
                        "NEW RECORD!"
                    } else {
                        "Personal Best kept"
                    }
                );
            }
        }
        _ => {}
    }
}

/// Renders the in-game Time Trial UI: the ESC menu modal and the live race HUD.
pub fn render_trial_ui(
    mut contexts: EguiContexts,
    mut manager: ResMut<TimeTrialManager>,
    mut car: Query<&mut Car>,
    mut chase_cam: Query<&mut ChaseCam>,
    mut current_map: ResMut<CurrentMap>,
) {
    let Ok(ctx) = contexts.ctx_mut() else { return };

    // Extract telemetry primitives upfront so we don't hold a long borrow on `car`
    let car_telemetry = car.iter().next().map(|c| {
        (
            c.speed(),
            c.lateral_speed,
            c.handbrake_input,
            c.grounded,
            c.placed,
            c.nitro_cooldown,
            c.nitro_timer,
            c.pos,
        )
    });

    // 1. IN-GAME HUD (Top bar when racing, waiting, or finished)
    if let Some(trial) = manager.current_trial().cloned() {
        let finish_bevy = wow_to_bevy(trial.end);
        let car_pos = car_telemetry.map(|t| t.7).unwrap_or(Vec3::ZERO);
        let dist_to_finish =
            Vec2::new(car_pos.x - finish_bevy.x, car_pos.z - finish_bevy.z).length();

        egui::Area::new(egui::Id::new("race_hud"))
            .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
            .show(ctx, |ui| {
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_black_alpha(200))
                    .stroke(egui::Stroke::new(
                        1.5_f32,
                        egui::Color32::from_rgb(255, 170, 0),
                    ))
                    .corner_radius(8.0)
                    .inner_margin(egui::Margin::symmetric(24, 12))
                    .show(ui, |ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading(
                                egui::RichText::new(&trial.name)
                                    .color(egui::Color32::WHITE)
                                    .strong(),
                            );
                            ui.add_space(4.0);

                            match manager.state {
                                TrialState::WaitingToStart => {
                                    ui.label(
                                        egui::RichText::new(
                                            "READY — PRESS 'W' / ACCELERATE TO START",
                                        )
                                        .color(egui::Color32::from_rgb(100, 255, 100))
                                        .size(20.0)
                                        .strong(),
                                    );
                                }
                                TrialState::Racing { elapsed } => {
                                    let mins = (elapsed / 60.0).floor() as u32;
                                    let secs = elapsed % 60.0;
                                    let time_str = format!("{mins:02}:{secs:05.2}");
                                    ui.label(
                                        egui::RichText::new(time_str)
                                            .color(egui::Color32::from_rgb(255, 230, 80))
                                            .size(28.0)
                                            .monospace()
                                            .strong(),
                                    );
                                }
                                TrialState::Finished {
                                    final_time,
                                    is_best,
                                } => {
                                    let mins = (final_time / 60.0).floor() as u32;
                                    let secs = final_time % 60.0;
                                    let time_str = format!("{mins:02}:{secs:05.2}");
                                    let msg = if is_best {
                                        format!("FINISHED! {time_str} ★ NEW RECORD! ★")
                                    } else {
                                        format!("FINISHED! {time_str}")
                                    };
                                    ui.label(
                                        egui::RichText::new(msg)
                                            .color(egui::Color32::from_rgb(50, 255, 120))
                                            .size(24.0)
                                            .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new("Press 'R' to retry, or ESC for menu")
                                            .color(egui::Color32::LIGHT_GRAY)
                                            .size(14.0),
                                    );
                                }
                                _ => {}
                            }

                            // Distance readout
                            let dist_km = dist_to_finish * 0.9144 / 1000.0;
                            ui.label(
                                egui::RichText::new(format!(
                                    "Finish: {:.0} yd ({:.2} km)",
                                    dist_to_finish, dist_km
                                ))
                                .color(egui::Color32::from_rgb(200, 200, 200))
                                .size(14.0),
                            );
                        });
                    });
            });
    }

    // 2. ESC MENU MODAL
    if manager.menu_open {
        let mut chosen_idx: Option<usize> = None;
        let selected = manager.selected_menu_idx;

        egui::Window::new("TIME TRIAL MENU")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_black_alpha(235))
                    .stroke(egui::Stroke::new(
                        2.0_f32,
                        egui::Color32::from_rgb(255, 140, 0),
                    ))
                    .corner_radius(12.0)
                    .inner_margin(egui::Margin::same(24)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(520.0);
                ui.vertical_centered(|ui| {
                    ui.heading(
                        egui::RichText::new("🏁 LAMBORGHINI TIME TRIALS 🏁")
                            .color(egui::Color32::from_rgb(255, 170, 0))
                            .size(26.0)
                            .strong(),
                    );
                    ui.label(
                        egui::RichText::new("Select a Trial or Free Driving • ESC to resume")
                            .color(egui::Color32::LIGHT_GRAY)
                            .size(14.0),
                    );
                });

                ui.add_space(16.0);
                ui.separator();
                ui.label(
                    egui::RichText::new("FREE DRIVING MODES:")
                        .color(egui::Color32::from_rgb(255, 140, 0))
                        .size(13.0)
                        .strong(),
                );
                ui.add_space(4.0);

                // Option 0: Free Driving (Crossroads)
                let is_sel0 = selected == 0;
                let is_active0 = manager.current_trial_idx.is_none() && current_map.0 == 1;
                let free0_text = format!(
                    "{} [1] The Barrens — Crossroads (Kalimdor) {}",
                    if is_sel0 { "▶ " } else { "   " },
                    if is_active0 { "(Active)" } else { "" }
                );

                let btn0 = ui.add_sized(
                    [ui.available_width(), 36.0],
                    egui::Button::new(
                        egui::RichText::new(free0_text)
                            .size(15.0)
                            .color(if is_sel0 {
                                egui::Color32::from_rgb(255, 220, 0)
                            } else {
                                egui::Color32::WHITE
                            })
                            .strong(),
                    )
                    .fill(if is_sel0 {
                        egui::Color32::from_rgb(60, 45, 10)
                    } else {
                        egui::Color32::from_black_alpha(100)
                    }),
                );
                if btn0.clicked() {
                    chosen_idx = Some(0);
                }

                ui.add_space(4.0);

                // Option 1: Free Driving (Goldshire)
                let is_sel1 = selected == 1;
                let is_active1 = manager.current_trial_idx.is_none() && current_map.0 == 0;
                let free1_text = format!(
                    "{} [2] Elwynn Forest — Goldshire (Eastern Kingdoms) {}",
                    if is_sel1 { "▶ " } else { "   " },
                    if is_active1 { "(Active)" } else { "" }
                );

                let btn1 = ui.add_sized(
                    [ui.available_width(), 36.0],
                    egui::Button::new(
                        egui::RichText::new(free1_text)
                            .size(15.0)
                            .color(if is_sel1 {
                                egui::Color32::from_rgb(255, 220, 0)
                            } else {
                                egui::Color32::WHITE
                            })
                            .strong(),
                    )
                    .fill(if is_sel1 {
                        egui::Color32::from_rgb(60, 45, 10)
                    } else {
                        egui::Color32::from_black_alpha(100)
                    }),
                );
                if btn1.clicked() {
                    chosen_idx = Some(1);
                }

                ui.add_space(10.0);
                ui.label(
                    egui::RichText::new("TIME TRIAL COURSES:")
                        .color(egui::Color32::from_rgb(255, 140, 0))
                        .size(13.0)
                        .strong(),
                );
                ui.add_space(4.0);

                // Trial Options (2..N+1)
                for (i, trial) in manager.trials.iter().enumerate() {
                    let option_idx = i + 2;
                    let is_sel = selected == option_idx;
                    let is_active = manager.current_trial_idx == Some(i);

                    let map_name = match trial.map_id {
                        0 => "Eastern Kingdoms",
                        1 => "Kalimdor",
                        other => &format!("Map {other}"),
                    };

                    let best_str = if let Some(b) = manager.best_times.get(&trial.name) {
                        let mins = (b / 60.0).floor() as u32;
                        let secs = b % 60.0;
                        format!(" ★ Best: {mins:02}:{secs:05.2}")
                    } else {
                        "".to_string()
                    };

                    let label_text = format!(
                        "{} [{}] {} ({}){}",
                        if is_sel { "▶ " } else { "   " },
                        option_idx + 1,
                        trial.name,
                        map_name,
                        if is_active { " (Active)" } else { "" }
                    );

                    let full_text = format!("{label_text}{best_str}");

                    let btn = ui.add_sized(
                        [ui.available_width(), 38.0],
                        egui::Button::new(egui::RichText::new(full_text).size(15.0).color(
                            if is_sel {
                                egui::Color32::from_rgb(255, 220, 0)
                            } else {
                                egui::Color32::WHITE
                            },
                        ))
                        .fill(if is_sel {
                            egui::Color32::from_rgb(60, 45, 10)
                        } else {
                            egui::Color32::from_black_alpha(100)
                        }),
                    );
                    if btn.clicked() {
                        chosen_idx = Some(option_idx);
                    }
                }

                ui.add_space(16.0);
                ui.separator();
                ui.add_space(12.0);

                // Footer controls guide and Close button
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(
                            "Controls: Up/Down or W/S to navigate, Enter to select, ESC to resume",
                        )
                        .color(egui::Color32::GRAY)
                        .size(12.0),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button(
                                egui::RichText::new("Close Menu")
                                    .size(14.0)
                                    .color(egui::Color32::LIGHT_GRAY),
                            )
                            .clicked()
                        {
                            manager.menu_open = false;
                        }
                    });
                });
            });

        if let Some(choice) = chosen_idx {
            apply_menu_choice(
                choice,
                &mut manager,
                &mut car,
                &mut chase_cam,
                &mut current_map,
            );
            manager.menu_open = false;
        }
    }

    // 3. DASHBOARD GAUGE CLUSTER (Speedometer & Nitro indicator)
    // Applies to BOTH Free Driving and Time Trials!
    if let Some((
        speed_yd_s,
        lateral_speed,
        handbrake,
        grounded,
        placed,
        nitro_cd,
        nitro_timer,
        _,
    )) = car_telemetry
    {
        let speed_kmh = (speed_yd_s * 0.9144 * 3.6).round() as i32;
        let speed_mph = (speed_yd_s * 0.9144 / 1609.344 * 3600.0).round() as i32;
        let is_boosting = nitro_timer > 0.0;
        let is_nitro_ready = nitro_cd <= 0.0;

        egui::Area::new(egui::Id::new("dashboard_hud"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-24.0, -24.0))
            .show(ctx, |ui| {
                let border_stroke = if is_boosting {
                    egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(0, 235, 255))
                } else {
                    egui::Stroke::new(1.5_f32, egui::Color32::from_rgb(255, 140, 0))
                };

                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_black_alpha(220))
                    .stroke(border_stroke)
                    .corner_radius(10.0)
                    .inner_margin(egui::Margin::symmetric(18, 14))
                    .show(ui, |ui| {
                        ui.set_min_width(220.0);

                        // Mode tag and status badges
                        ui.horizontal(|ui| {
                            if manager.current_trial_idx.is_some() {
                                ui.label(
                                    egui::RichText::new("🏁 TRIAL")
                                        .color(egui::Color32::from_rgb(255, 180, 0))
                                        .size(11.0)
                                        .strong(),
                                );
                            } else {
                                ui.label(
                                    egui::RichText::new("🌍 FREE DRIVE")
                                        .color(egui::Color32::from_rgb(80, 220, 160))
                                        .size(11.0)
                                        .strong(),
                                );
                            }

                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if handbrake {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 60, 60),
                                            "BRAKE",
                                        );
                                    } else if lateral_speed > 3.2 {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 165, 0),
                                            "DRIFT",
                                        );
                                    } else if !grounded && placed {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(100, 200, 255),
                                            "AIR",
                                        );
                                    }
                                },
                            );
                        });

                        ui.add_space(4.0);

                        // Speedometer readout
                        let speed_color = if is_boosting {
                            egui::Color32::from_rgb(0, 255, 255)
                        } else if speed_kmh >= 320 {
                            egui::Color32::from_rgb(255, 50, 90)
                        } else if speed_kmh >= 220 {
                            egui::Color32::from_rgb(255, 120, 30)
                        } else if speed_kmh >= 120 {
                            egui::Color32::from_rgb(255, 220, 50)
                        } else {
                            egui::Color32::from_rgb(240, 245, 255)
                        };

                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!("{:>3}", speed_kmh))
                                    .color(speed_color)
                                    .size(44.0)
                                    .monospace()
                                    .strong(),
                            );
                            ui.vertical(|ui| {
                                ui.add_space(8.0);
                                ui.label(
                                    egui::RichText::new("KM/H")
                                        .color(egui::Color32::WHITE)
                                        .size(13.0)
                                        .strong(),
                                );
                                ui.label(
                                    egui::RichText::new(format!("{speed_mph} MPH"))
                                        .color(egui::Color32::LIGHT_GRAY)
                                        .size(11.0),
                                );
                            });
                        });

                        ui.add_space(4.0);

                        // Speed gauge progress bar (0 to 360 km/h)
                        let speed_ratio = (speed_kmh as f32 / 360.0).clamp(0.0, 1.0);
                        let bar_width = ui.available_width();
                        let (speed_bar_rect, _) = ui
                            .allocate_exact_size(egui::vec2(bar_width, 6.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            speed_bar_rect,
                            3.0,
                            egui::Color32::from_black_alpha(160),
                        );
                        if speed_ratio > 0.0 {
                            let filled_rect = egui::Rect::from_min_size(
                                speed_bar_rect.min,
                                egui::vec2(
                                    speed_bar_rect.width() * speed_ratio,
                                    speed_bar_rect.height(),
                                ),
                            );
                            ui.painter().rect_filled(filled_rect, 3.0, speed_color);
                        }

                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);

                        // Nitro indicator section
                        let (nitro_label, nitro_color, nitro_fill_color, nitro_ratio) =
                            if is_boosting {
                                (
                                    "⚡ NITRO BOOST ACTIVE!".to_string(),
                                    egui::Color32::from_rgb(0, 255, 255),
                                    egui::Color32::from_rgb(0, 240, 255),
                                    (nitro_timer / 1.0).clamp(0.0, 1.0),
                                )
                            } else if is_nitro_ready {
                                (
                                    "⚡ NITRO READY [SHIFT]".to_string(),
                                    egui::Color32::from_rgb(60, 255, 140),
                                    egui::Color32::from_rgb(40, 230, 130),
                                    1.0_f32,
                                )
                            } else {
                                (
                                    format!("⚡ NITRO CHARGING ({:.1}s)", nitro_cd),
                                    egui::Color32::from_rgb(150, 190, 230),
                                    egui::Color32::from_rgb(30, 130, 230),
                                    ((10.0 - nitro_cd) / 10.0).clamp(0.0, 1.0),
                                )
                            };

                        ui.label(
                            egui::RichText::new(nitro_label)
                                .color(nitro_color)
                                .size(12.0)
                                .strong(),
                        );

                        ui.add_space(4.0);

                        // Nitro gauge bar
                        let (nitro_bar_rect, _) = ui
                            .allocate_exact_size(egui::vec2(bar_width, 8.0), egui::Sense::hover());
                        ui.painter().rect_filled(
                            nitro_bar_rect,
                            4.0,
                            egui::Color32::from_black_alpha(180),
                        );
                        if nitro_ratio > 0.0 {
                            let filled_rect = egui::Rect::from_min_size(
                                nitro_bar_rect.min,
                                egui::vec2(
                                    nitro_bar_rect.width() * nitro_ratio,
                                    nitro_bar_rect.height(),
                                ),
                            );
                            ui.painter().rect_filled(filled_rect, 4.0, nitro_fill_color);
                        }
                    });
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_default_json() {
        let json_str = include_str!("../assets/time_trials.json");
        let defs: Vec<TimeTrialDef> =
            serde_json::from_str(json_str).expect("parse time_trials.json");
        assert!(!defs.is_empty());
        let trials: Vec<TimeTrial> = defs.into_iter().map(Into::into).collect();
        assert_eq!(trials.len(), 4);
        assert_eq!(trials[0].name, "Crossroads to Ratchet Sprint");
        assert_eq!(trials[0].map_id, 1);
        assert_eq!(trials[0].start, [-437.0, -2596.0, 96.0]);
        assert_eq!(trials[0].end, [-850.0, -3650.0, 30.0]);
        assert_eq!(trials[0].rotation, Some(115.0));

        assert_eq!(trials[1].name, "Lakeshire to Sentinel Hill");
        assert_eq!(trials[1].map_id, 0);
        assert_eq!(trials[1].rotation, Some(-90.0));
    }

    #[test]
    fn test_parse_flexible_coords_and_rotation() {
        let json_str = r#"[
            {
                "name": "Object Coords",
                "map_id": 1,
                "start": {"x": 10.0, "y": 20.0, "z": 30.0},
                "end": [40.0, 50.0, 60.0],
                "yaw": 45.0
            }
        ]"#;
        let defs: Vec<TimeTrialDef> = serde_json::from_str(json_str).unwrap();
        let trial: TimeTrial = defs.into_iter().next().unwrap().into();
        assert_eq!(trial.start, [10.0, 20.0, 30.0]);
        assert_eq!(trial.end, [40.0, 50.0, 60.0]);
        assert_eq!(trial.rotation, Some(45.0));
    }
}
