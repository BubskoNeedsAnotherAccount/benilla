//! Time trial gamemode and race management.
//!
//! Loads races from a JSON file (`time_trials.json`), tracks start and finish coordinates,
//! map switches across continents (e.g. Kalimdor <-> Eastern Kingdoms), renders in-world
//! 3D start/finish beacon markers, provides an ESC menu to choose a race or free driving,
//! and tracks race timers and personal bests.

use std::collections::HashMap;

use crate::settings::{MenuTab, Settings, RENDER_SCALES};
use benilla_world::view::FARCLIP_RANGE;
use bevy::asset::RenderAssetUsages;
use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::world_map::CurrentMap;

use crate::camera::ChaseCam;
use crate::car::Car;

use crate::car::{
    BLOODHOOF_WOW, CROSSROADS_WOW, DARKSHIRE_WOW, GADGETZAN_WOW, GOLDSHIRE_WOW, KHARANOS_WOW,
    LAKESHIRE_WOW, RAZOR_HILL_WOW, SENTINEL_HILL_WOW, SOUTHSHORE_WOW,
};

/// Horizontal radius, in yards, of a checkpoint or finish gate unless a course says otherwise.
pub const DEFAULT_GATE_RADIUS: f32 = 16.0;
/// A gate is passed within this many yards above or below its coordinate.
const GATE_HEIGHT: f32 = 45.0;

fn default_gate_radius() -> f32 {
    DEFAULT_GATE_RADIUS
}

fn one_lap() -> u32 {
    1
}

/// A single course definition: a point-to-point time trial, or a race with checkpoints and laps.
///
/// In `time_trials.json` a course is an object:
///
/// ```json
/// {
///   "name": "Lakeshire Circuit",
///   "map_id": 0,
///   "start": [-9429.1, -2231.4, 68.65],
///   "rotation": 270,
///   "checkpoints": [[-9500, -2100, 65], {"x": -9600, "y": -2200, "z": 60}],
///   "loop": true,
///   "laps": 3,
///   "checkpoint_radius": 20
/// }
/// ```
///
/// - `checkpoints` are passed in order, every lap. Omit them for a plain time trial.
/// - `loop: true` (or leaving out `end`) makes a circuit: the start is also the finish line, and
///   a lap ends when the car crosses it after the last checkpoint.
/// - `laps` repeats a circuit; a point-to-point course always has one.
/// - `checkpoint_radius` sizes every gate of the course (yards, default 16).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeTrial {
    pub name: String,
    pub map_id: u32,
    pub start: [f32; 3], // WoW coordinates: [x north, y west, z up]
    /// The finish line; the start itself for a circuit.
    pub end: [f32; 3], // WoW coordinates: [x north, y west, z up]
    /// Optional starting rotation in degrees (if None, faces the first checkpoint, else the finish).
    #[serde(default)]
    pub rotation: Option<f32>,
    /// Gates to pass, in order, before each finish crossing.
    #[serde(default)]
    pub checkpoints: Vec<[f32; 3]>,
    #[serde(default = "one_lap")]
    pub laps: u32,
    /// The start is the finish line.
    #[serde(default)]
    pub circuit: bool,
    #[serde(default = "default_gate_radius")]
    pub checkpoint_radius: f32,
    /// Online, the drivers on this course line up on a grid and start together.
    #[serde(default)]
    pub versus: bool,
}

impl TimeTrial {
    /// A point-to-point sprint with no checkpoints.
    pub fn sprint(
        name: &str,
        map_id: u32,
        start: [f32; 3],
        end: [f32; 3],
        rotation: Option<f32>,
    ) -> Self {
        Self {
            name: name.into(),
            map_id,
            start,
            end,
            rotation,
            checkpoints: Vec::new(),
            laps: 1,
            circuit: false,
            checkpoint_radius: DEFAULT_GATE_RADIUS,
            versus: false,
        }
    }

    /// Laps to complete, at least one.
    pub fn lap_count(&self) -> u32 {
        self.laps.max(1)
    }

    /// Whether the course has checkpoints or more than one lap, so the HUD tracks laps.
    pub fn is_race(&self) -> bool {
        !self.checkpoints.is_empty() || self.lap_count() > 1
    }

    /// The first gate the car should drive at from the start.
    fn first_target(&self) -> [f32; 3] {
        self.checkpoints.first().copied().unwrap_or(self.end)
    }
}

/// Where the car is on the course.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RaceProgress {
    /// Checkpoints passed this lap.
    pub next_checkpoint: usize,
    /// Race clock when the current lap began.
    pub lap_start: f32,
    /// Finished laps, in order.
    pub lap_times: Vec<f32>,
    /// The car has left the finish gate this lap, so crossing it counts. A circuit starts inside it.
    pub armed: bool,
}

/// Whether a car at `pos` is inside the gate at WoW coordinates `gate`.
fn in_gate(pos: Vec3, gate: [f32; 3], radius: f32) -> bool {
    let g = wow_to_bevy(gate);
    Vec2::new(pos.x - g.x, pos.z - g.z).length() <= radius && (pos.y - g.y).abs() <= GATE_HEIGHT
}

/// Takes the gates a car at `pos` is in, and says whether a lap just ended. Checkpoints count
/// only in order, and the finish only once every checkpoint of the lap is passed. A circuit
/// starts inside its finish gate, so the car must leave it once per lap.
fn pass_gates(trial: &TimeTrial, progress: &mut RaceProgress, pos: Vec3) -> bool {
    let radius = trial.checkpoint_radius;
    if let Some(gate) = trial.checkpoints.get(progress.next_checkpoint) {
        if in_gate(pos, *gate, radius) {
            progress.next_checkpoint += 1;
        }
    }
    let at_finish = in_gate(pos, trial.end, radius);
    if !at_finish {
        progress.armed = true;
    }
    at_finish && progress.armed && progress.next_checkpoint >= trial.checkpoints.len()
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
    /// The finish line; a course without one is a circuit finishing at its start.
    #[serde(
        default,
        alias = "end_coords",
        alias = "end_coord",
        alias = "end_pos",
        alias = "end_position",
        alias = "finish"
    )]
    pub end: Option<CoordDef>,
    #[serde(default, alias = "gates", alias = "waypoints")]
    pub checkpoints: Vec<CoordDef>,
    #[serde(default, alias = "lap_count")]
    pub laps: Option<u32>,
    #[serde(default, rename = "loop", alias = "circuit")]
    pub is_loop: bool,
    #[serde(default, alias = "gate_radius", alias = "radius")]
    pub checkpoint_radius: Option<f32>,
    /// Starting rotation / heading in degrees (finetunable in JSON).
    #[serde(
        default,
        alias = "yaw",
        alias = "rotation_deg",
        alias = "heading",
        alias = "start_rotation"
    )]
    pub rotation: Option<f32>,
    #[serde(default, alias = "vs")]
    pub versus: bool,
}

impl From<TimeTrialDef> for TimeTrial {
    fn from(def: TimeTrialDef) -> Self {
        let start = def.start.to_array();
        let circuit = def.is_loop || def.end.is_none();
        let end = if circuit {
            start
        } else {
            def.end.map_or(start, |e| e.to_array())
        };
        let mut laps = def.laps.unwrap_or(1).max(1);
        if laps > 1 && !circuit {
            eprintln!(
                "benilla-lambo: '{}' has laps but no loop; running a single lap",
                def.name
            );
            laps = 1;
        }
        Self {
            name: def.name,
            map_id: def.map_id,
            start,
            end,
            rotation: def.rotation,
            checkpoints: def.checkpoints.iter().map(CoordDef::to_array).collect(),
            laps,
            circuit,
            checkpoint_radius: def
                .checkpoint_radius
                .filter(|r| *r > 0.0)
                .unwrap_or(DEFAULT_GATE_RADIUS),
            versus: def.versus,
        }
    }
}

/// State of the active time trial.
#[derive(Debug, Clone, PartialEq)]
pub enum TrialState {
    FreeDriving,
    WaitingToStart,
    /// A versus start: every car on the grid is held until the count runs out.
    Countdown {
        remaining: f32,
    },
    Racing {
        elapsed: f32,
    },
    Finished {
        final_time: f32,
        is_best: bool,
    },
}

/// Global resource managing all time trials, race progress, records, and menu state.
#[derive(Resource)]
pub struct TimeTrialManager {
    pub trials: Vec<TimeTrial>,
    pub current_trial_idx: Option<usize>,
    pub state: TrialState,
    pub progress: RaceProgress,
    pub best_times: HashMap<String, f32>,
    pub best_laps: HashMap<String, f32>,
    pub menu_open: bool,
    pub selected_menu_idx: usize,
    /// Index into `FREE_DRIVES`.
    pub free_drive_loc: usize,
    pub debug_open: bool,
    /// The rivals already on this countdown's grid; a new one restarts the count.
    pub versus_known: Vec<u32>,
    /// Our place on the grid and the grid's size when the car was last put there.
    pub grid_slot: Option<(usize, usize)>,
    /// The picked course's map is still being preloaded (`preload::mark_course_loading`): the car
    /// is held and no start or countdown begins until it has loaded.
    pub loading: bool,
}

/// Seconds a versus start counts down, from the last driver to join the grid.
pub const VERSUS_COUNTDOWN: f32 = 10.0;
/// Grid spacing: across the road, and back per place.
const GRID_SIDE: f32 = 2.6;
const GRID_BACK: f32 = 4.0;

/// Multiplayer as the race code sees it. The browser build's `net` fills it; elsewhere it stays
/// offline and a versus course runs as a plain one.
#[derive(Resource, Default)]
pub struct VersusLobby {
    /// Connected to a relay.
    pub online: bool,
    /// This client's relay id.
    pub my_id: Option<u32>,
    /// The other drivers counting down on the same course, sorted by id.
    pub rivals: Vec<u32>,
}

impl TimeTrialManager {
    /// The versus course this car is on, and whether its start is counting down. Only the
    /// browser build has a relay to tell.
    #[cfg_attr(not(target_arch = "wasm32"), allow(dead_code))]
    pub fn versus_tag(&self) -> Option<(&str, bool)> {
        let trial = self.current_trial().filter(|t| t.versus)?;
        match self.state {
            TrialState::Countdown { .. } => Some((&trial.name, true)),
            TrialState::Racing { .. } | TrialState::WaitingToStart => Some((&trial.name, false)),
            _ => None,
        }
    }

    /// The car is held still: a versus countdown is running, or the course is still loading.
    pub fn frozen(&self) -> bool {
        match self.state {
            TrialState::Countdown { .. } => true,
            TrialState::WaitingToStart => self.loading,
            _ => false,
        }
    }}

impl Default for TimeTrialManager {
    fn default() -> Self {
        let trials = load_time_trials();
        let is_kalimdor = std::env::var("WOW_MAP").map(|v| v == "1").unwrap_or(false);
        let free_drive_loc = if is_kalimdor { 1 } else { 0 };
        Self {
            trials,
            current_trial_idx: None,
            state: TrialState::FreeDriving,
            progress: RaceProgress::default(),
            best_times: HashMap::new(),
            best_laps: HashMap::new(),
            menu_open: false,
            selected_menu_idx: 0,
            free_drive_loc,
            debug_open: false,
            versus_known: Vec::new(),
            grid_slot: None,
            loading: false,
        }
    }
}

impl TimeTrialManager {
    /// Returns the currently active time trial, if one is selected.
    pub fn current_trial(&self) -> Option<&TimeTrial> {
        self.current_trial_idx.and_then(|idx| self.trials.get(idx))
    }

    /// The gate the car is driving at now: the next checkpoint, else the finish.
    pub fn target_gate(&self) -> Option<[f32; 3]> {
        let trial = self.current_trial()?;
        Some(
            trial
                .checkpoints
                .get(self.progress.next_checkpoint)
                .copied()
                .unwrap_or(trial.end),
        )
    }

    /// "Lap 2/3 · CP 1/4" for a race, empty for a plain time trial.
    pub fn race_label(&self) -> String {
        let Some(trial) = self.current_trial().filter(|t| t.is_race()) else {
            return String::new();
        };
        let lap = (self.progress.lap_times.len() as u32 + 1).min(trial.lap_count());
        let mut label = format!("Lap {lap}/{}", trial.lap_count());
        if !trial.checkpoints.is_empty() {
            label.push_str(&format!(
                " · CP {}/{}",
                self.progress.next_checkpoint,
                trial.checkpoints.len()
            ));
        }
        label
    }
}

/// Formats seconds as `MM:SS.cc`.
fn format_time(secs: f32) -> String {
    format!("{:02}:{:05.2}", (secs / 60.0).floor() as u32, secs % 60.0)
}

/// Discovers and loads time trials from JSON files or environment variable.
pub fn load_time_trials() -> Vec<TimeTrial> {
    // A browser has no disk to look on: the shipped trials are compiled in.
    #[cfg(target_arch = "wasm32")]
    if let Ok(defs) =
        serde_json::from_str::<Vec<TimeTrialDef>>(include_str!("../assets/time_trials.json"))
    {
        let trials: Vec<TimeTrial> = defs.into_iter().map(Into::into).collect();
        if !trials.is_empty() {
            return trials;
        }
    }
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
        TimeTrial::sprint(
            "Crossroads to Ratchet Sprint",
            1,
            [-437.0, -2596.0, 96.0],
            [-850.0, -3650.0, 30.0],
            Some(115.0),
        ),
        TimeTrial::sprint(
            "Lakeshire to Sentinel Hill",
            0,
            [-9429.1, -2231.4, 68.65],
            [-10628.89, 1036.68, 34.06],
            Some(-90.0),
        ),
        TimeTrial::sprint(
            "Menethil Harbor to Thelsamar",
            0,
            [-3792.26, -783.29, 9.06],
            [-5421.91, -2930.01, 347.25],
            Some(90.0),
        ),
        TimeTrial::sprint(
            "Caverns of Time to The Great Lift",
            1,
            [-8180.0, -4460.0, 8.0],
            [-4945.0, -1350.0, 85.0],
            Some(45.0),
        ),
    ]
}

/// Root of a gate built for the current course: a checkpoint by index, or `None` the finish.
#[derive(Component)]
pub struct GateRoot {
    index: Option<usize>,
}

/// A part of a checkpoint gate, lit while its gate is the next one and dim while it is the one after.
#[derive(Component)]
pub struct GatePart {
    index: usize,
    beam: bool,
}

/// Shared look of the course gates.
#[derive(Resource)]
pub struct GateAssets {
    hot: Handle<StandardMaterial>,
    dim: Handle<StandardMaterial>,
    beam_hot: Handle<StandardMaterial>,
    beam_dim: Handle<StandardMaterial>,
    finish_post: Handle<StandardMaterial>,
    checker: Handle<StandardMaterial>,
    finish_beam: Handle<StandardMaterial>,
    post: Handle<Mesh>,
    beam: Handle<Mesh>,
}

/// A gate's posts, yards; their feet sink below the gate point to meet sloped ground.
const POST_HEIGHT: f32 = 10.0;
const POST_SINK: f32 = 1.5;
/// The light column over a gate, seen from far off.
const BEAM_HEIGHT: f32 = 220.0;

/// A black and white checker, 16 squares by 2, for the finish banner and line.
fn checker_image() -> Image {
    const W: u32 = 16;
    const H: u32 = 2;
    let mut data = Vec::with_capacity((W * H * 4) as usize);
    for y in 0..H {
        for x in 0..W {
            let v = if (x + y) % 2 == 0 { 240 } else { 18 };
            data.extend_from_slice(&[v, v, v, 255]);
        }
    }
    let mut image = Image::new(
        Extent3d {
            width: W,
            height: H,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    image.sampler = ImageSampler::nearest();
    image
}

/// Builds the materials and meshes every course's gates share.
pub fn setup_markers(
    mut commands: Commands,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<StandardMaterial>>,
    mut images: ResMut<Assets<Image>>,
) {
    let mut glow = |color: Color, blend: bool| {
        materials.add(StandardMaterial {
            base_color: color,
            unlit: true,
            cull_mode: None,
            alpha_mode: if blend {
                AlphaMode::Blend
            } else {
                AlphaMode::Opaque
            },
            ..default()
        })
    };
    let hot = glow(Color::srgb(0.15, 0.9, 1.0), false);
    let dim = glow(Color::srgba(0.15, 0.9, 1.0, 0.35), true);
    let beam_hot = glow(Color::srgba(0.15, 0.9, 1.0, 0.22), true);
    let beam_dim = glow(Color::srgba(0.15, 0.9, 1.0, 0.07), true);
    let finish_post = glow(Color::srgb(1.0, 0.78, 0.1), false);
    let finish_beam = glow(Color::srgba(1.0, 0.8, 0.1, 0.25), true);
    let checker = materials.add(StandardMaterial {
        base_color_texture: Some(images.add(checker_image())),
        unlit: true,
        cull_mode: None,
        ..default()
    });
    commands.insert_resource(GateAssets {
        hot,
        dim,
        beam_hot,
        beam_dim,
        finish_post,
        checker,
        finish_beam,
        post: meshes.add(Cylinder::new(0.8, POST_HEIGHT)),
        beam: meshes.add(Cylinder::new(1.2, BEAM_HEIGHT)),
    });
}

/// The flat direction a car crosses gate `index` (`None` the finish) in, Bevy space: from the
/// gate before it.
fn gate_heading(trial: &TimeTrial, index: Option<usize>) -> Vec3 {
    let to = index.map_or(trial.end, |i| trial.checkpoints[i]);
    let from = match index {
        Some(0) => trial.start,
        Some(i) => trial.checkpoints[i - 1],
        None => trial.checkpoints.last().copied().unwrap_or(trial.start),
    };
    let d = wow_to_bevy(to) - wow_to_bevy(from);
    Vec3::new(d.x, 0.0, d.z).try_normalize().unwrap_or_else(|| {
        let yaw = start_yaw(trial);
        Vec3::new(-yaw.sin(), 0.0, -yaw.cos())
    })
}

/// A gate at WoW point `gate`, its posts across the way a car arrives.
fn gate_transform(gate: [f32; 3], heading: Vec3) -> Transform {
    Transform::from_translation(wow_to_bevy(gate)).looking_to(heading, Vec3::Y)
}

/// Spawns the checkpoint arches and the finish line of a course.
fn build_course(
    commands: &mut Commands,
    assets: &GateAssets,
    meshes: &mut Assets<Mesh>,
    trial: &TimeTrial,
) {
    let r = trial.checkpoint_radius;
    let top = POST_HEIGHT - POST_SINK;
    let bar = meshes.add(Cuboid::new(2.0 * r + 1.6, 1.1, 1.1));
    let ring = meshes.add(Torus::new(r - 0.35, r + 0.35));
    let banner_h = r / 4.0;
    let banner = meshes.add(Cuboid::new(2.0 * r, banner_h, 0.3));
    let line = meshes.add(Cuboid::new(2.0 * r, 0.08, banner_h));

    // On a circuit's laps before the last, the line is one more checkpoint: an arch indexed after
    // the course's own gates, standing where the finish does.
    let n = trial.checkpoints.len();
    let lap_line = (trial.lap_count() > 1).then(|| (n, trial.end, gate_heading(trial, None)));
    let arches = trial
        .checkpoints
        .iter()
        .enumerate()
        .map(|(i, gate)| (i, *gate, gate_heading(trial, Some(i))))
        .chain(lap_line);
    for (i, gate, heading) in arches {
        let at = gate_transform(gate, heading);
        commands
            .spawn((GateRoot { index: Some(i) }, at, Visibility::Hidden))
            .with_children(|p| {
                let solid = |mesh: &Handle<Mesh>, at: Transform| {
                    (
                        GatePart {
                            index: i,
                            beam: false,
                        },
                        Mesh3d(mesh.clone()),
                        MeshMaterial3d(assets.dim.clone()),
                        at,
                    )
                };
                for side in [-r, r] {
                    p.spawn(solid(
                        &assets.post,
                        Transform::from_xyz(side, POST_HEIGHT / 2.0 - POST_SINK, 0.0),
                    ));
                }
                p.spawn(solid(&bar, Transform::from_xyz(0.0, top, 0.0)));
                // The ring on the ground is the gate's reach: drive through it.
                p.spawn(solid(&ring, Transform::from_xyz(0.0, -0.3, 0.0)));
                p.spawn((
                    GatePart {
                        index: i,
                        beam: true,
                    },
                    Mesh3d(assets.beam.clone()),
                    MeshMaterial3d(assets.beam_dim.clone()),
                    Transform::from_xyz(0.0, top + BEAM_HEIGHT / 2.0, 0.0),
                ));
            });
    }

    let at = gate_transform(trial.end, gate_heading(trial, None));
    commands
        .spawn((GateRoot { index: None }, at, Visibility::Visible))
        .with_children(|p| {
            for side in [-r, r] {
                p.spawn((
                    Mesh3d(assets.post.clone()),
                    MeshMaterial3d(assets.finish_post.clone()),
                    Transform::from_xyz(side, POST_HEIGHT / 2.0 - POST_SINK, 0.0),
                ));
            }
            p.spawn((
                Mesh3d(banner),
                MeshMaterial3d(assets.checker.clone()),
                Transform::from_xyz(0.0, top - banner_h / 2.0, 0.0),
            ));
            p.spawn((
                Mesh3d(line),
                MeshMaterial3d(assets.checker.clone()),
                Transform::from_xyz(0.0, -0.4, 0.0),
            ));
            p.spawn((
                Mesh3d(assets.beam.clone()),
                MeshMaterial3d(assets.finish_beam.clone()),
                Transform::from_xyz(0.0, top + BEAM_HEIGHT / 2.0, 0.0),
            ));
        });
}

/// Whether gate `index` (`None` the checkered finish) stands, with `next` the gate driven at,
/// `last` the course's checkpoint count (the lap-line arch's index), on the last lap or not, and
/// the course `live` (not free driving or finished).
fn gate_shown(index: Option<usize>, next: usize, last: usize, last_lap: bool, live: bool) -> bool {
    match index {
        // The checkered finish stands only on the last lap.
        None => last_lap,
        // Before it, the line is an arch at index `last`.
        Some(i) if i == last => live && !last_lap && (next == last || next + 1 == last),
        // Driving at the lap line, the next lap's first gate shows dim behind it.
        Some(0) if next == last && !last_lap => live,
        Some(i) => live && (i == next || i == next + 1),
    }
}

/// Rebuilds the gates when the course changes. The next checkpoint is lit and the one after it
/// shown dim; the checkered finish stands on the last lap, and before it the line is an arch.
#[allow(clippy::type_complexity)]
pub fn update_course_markers(
    mut commands: Commands,
    manager: Res<TimeTrialManager>,
    assets: Option<Res<GateAssets>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut roots: Query<(Entity, &GateRoot, &mut Visibility)>,
    mut parts: Query<(&GatePart, &mut MeshMaterial3d<StandardMaterial>)>,
    mut built_for: Local<Option<usize>>,
) {
    let Some(assets) = assets else { return };
    let course = manager.current_trial_idx;
    if *built_for != course {
        for (entity, _, _) in &roots {
            commands.entity(entity).despawn();
        }
        *built_for = course;
        if let Some(trial) = manager.current_trial() {
            build_course(&mut commands, &assets, &mut meshes, trial);
        }
        // The new gates exist next frame; their visibility is set then.
        return;
    }

    let live = matches!(
        manager.state,
        TrialState::WaitingToStart | TrialState::Countdown { .. } | TrialState::Racing { .. }
    );
    let next = manager.progress.next_checkpoint;
    let Some(trial) = manager.current_trial() else {
        return;
    };
    let last = trial.checkpoints.len();
    let last_lap = manager.progress.lap_times.len() as u32 + 1 >= trial.lap_count();
    for (_, root, mut vis) in &mut roots {
        let show = gate_shown(root.index, next, last, last_lap, live);
        vis.set_if_neq(if show {
            Visibility::Visible
        } else {
            Visibility::Hidden
        });
    }
    for (part, mut mat) in &mut parts {
        let want = match (part.beam, part.index == next) {
            (false, true) => &assets.hot,
            (false, false) => &assets.dim,
            (true, true) => &assets.beam_hot,
            (true, false) => &assets.beam_dim,
        };
        if mat.0 != *want {
            mat.0 = want.clone();
        }
    }
}

/// The heading a car starts a course at: its `rotation`, else toward the first gate.
fn start_yaw(trial: &TimeTrial) -> f32 {
    trial.rotation.map(f32::to_radians).unwrap_or_else(|| {
        let d = wow_to_bevy(trial.first_target()) - wow_to_bevy(trial.start);
        (-d.x).atan2(-d.z)
    })
}

/// Where R puts the car mid-race: the last checkpoint passed this lap (raised for the drop, as the
/// start is), facing the gate after it. `None` before the first checkpoint, where R restarts.
fn checkpoint_respawn(trial: &TimeTrial, progress: &RaceProgress) -> Option<([f32; 3], f32)> {
    let last = *trial
        .checkpoints
        .get(progress.next_checkpoint.checked_sub(1)?)?;
    let next = trial
        .checkpoints
        .get(progress.next_checkpoint)
        .copied()
        .unwrap_or(trial.end);
    let d = wow_to_bevy(next) - wow_to_bevy(last);
    Some(([last[0], last[1], last[2] + 40.0], (-d.x).atan2(-d.z)))
}

/// Where versus grid place `slot` starts (WoW coordinates, raised for the drop) and its heading:
/// a staggered two-wide grid behind the start, every car facing the course's start heading.
pub fn grid_spawn(trial: &TimeTrial, slot: usize) -> ([f32; 3], f32) {
    let yaw = start_yaw(trial);
    let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
    let right = fwd.cross(Vec3::Y);
    let side = if slot % 2 == 0 { -GRID_SIDE } else { GRID_SIDE };
    let at = wow_to_bevy(trial.start) - fwd * (slot as f32 * GRID_BACK) + right * side;
    let mut wow = bevy_to_wow(at);
    wow[2] = trial.start[2] + 40.0;
    (wow, yaw)
}

/// Runs a versus start: online, a versus course counts down instead of waiting for
/// the throttle, puts the car on its grid place, and restarts the count whenever a driver joins
/// the grid.
pub fn update_versus(
    time: Res<Time>,
    mut manager: ResMut<TimeTrialManager>,
    lobby: Res<VersusLobby>,
    mut car: Query<&mut Car>,
    mut chase_cam: Query<&mut ChaseCam>,
) {
    let Some(trial) = manager.current_trial().filter(|t| t.versus).cloned() else {
        return;
    };
    // The count starts, and with it the join signal other drivers see, only once the map is in.
    if manager.state == TrialState::WaitingToStart && lobby.online && !manager.loading {
        manager.state = TrialState::Countdown {
            remaining: VERSUS_COUNTDOWN,
        };
        manager.versus_known.clear();
        manager.grid_slot = None;
    }
    let TrialState::Countdown { mut remaining } = manager.state else {
        return;
    };
    if !lobby.online {
        // Went offline: an ordinary start.
        manager.state = TrialState::WaitingToStart;
        return;
    }

    let joined = lobby
        .rivals
        .iter()
        .any(|id| !manager.versus_known.contains(id));
    if joined {
        remaining = VERSUS_COUNTDOWN;
    }
    manager.versus_known = lobby.rivals.clone();

    // Places go by relay id, so a driver who joins later lines up behind. A car is moved only
    // when someone joins: one leaving the grid (because their count ran out first) moves no one.
    if joined || manager.grid_slot.is_none() {
        let mut grid = lobby.rivals.clone();
        grid.extend(lobby.my_id);
        grid.sort_unstable();
        grid.dedup();
        let slot = lobby
            .my_id
            .and_then(|me| grid.iter().position(|&id| id == me))
            .unwrap_or(0);
        let place = (slot, grid.len());
        if manager.grid_slot.map(|(s, _)| s) != Some(slot) {
            let (at, yaw) = grid_spawn(&trial, slot);
            if let Ok(mut c) = car.single_mut() {
                c.set_spawn(at, yaw);
            }
            if let Ok(mut cam) = chase_cam.single_mut() {
                cam.yaw = yaw;
                cam.snapped = false;
            }
        }
        manager.grid_slot = Some(place);
    }

    // The count holds until the car is on the ground.
    if car.single().is_ok_and(|c| c.placed) && !manager.loading {
        remaining -= time.delta_secs();
    }
    manager.state = if remaining <= 0.0 {
        manager.progress = RaceProgress::default();
        TrialState::Racing { elapsed: 0.0 }
    } else {
        TrialState::Countdown { remaining }
    };
}

/// Handles ESC menu toggle, keyboard navigation, and restarts with 'R'.
pub fn handle_trial_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut manager: ResMut<TimeTrialManager>,
    mut car: Query<&mut Car>,
    mut chase_cam: Query<&mut ChaseCam>,
    mut current_map: ResMut<CurrentMap>,
    mut tab: ResMut<MenuTab>,
) {
    // ESC toggles the menu
    if keys.just_pressed(KeyCode::Escape) {
        manager.menu_open = !manager.menu_open;
    }
    // Tab flips between the menu's pages.
    if manager.menu_open && keys.just_pressed(KeyCode::Tab) {
        *tab = match *tab {
            MenuTab::FreeDrive => MenuTab::Race,
            MenuTab::Race => MenuTab::Settings,
            MenuTab::Settings => MenuTab::FreeDrive,
        };
        if let Some(range) = menu_range(*tab, manager.trials.len()) {
            manager.selected_menu_idx = range.start;
        }
    }

    // 'G' toggles the debug telemetry overlay
    if keys.just_pressed(KeyCode::KeyG) {
        manager.debug_open = !manager.debug_open;
    }

    // When menu is open, handle keyboard navigation
    if manager.menu_open {
        // The settings page is driven with the mouse; the keys would pick a course.
        let Some(range) = menu_range(*tab, manager.trials.len()) else {
            return;
        };
        if range.is_empty() {
            return;
        }
        // The selection belongs to the page showing.
        if !range.contains(&manager.selected_menu_idx) {
            manager.selected_menu_idx = range.start;
        }
        if keys.just_pressed(KeyCode::ArrowUp) || keys.just_pressed(KeyCode::KeyW) {
            if manager.selected_menu_idx == range.start {
                manager.selected_menu_idx = range.end - 1;
            } else {
                manager.selected_menu_idx -= 1;
            }
        }
        if keys.just_pressed(KeyCode::ArrowDown) || keys.just_pressed(KeyCode::KeyS) {
            manager.selected_menu_idx = if manager.selected_menu_idx + 1 >= range.end {
                range.start
            } else {
                manager.selected_menu_idx + 1
            };
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

        // Quick numeric selection: 1 is the first row of the page showing.
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
            if keys.just_pressed(*key) && range.start + i < range.end {
                apply_menu_choice(
                    range.start + i,
                    &mut manager,
                    &mut car,
                    &mut chase_cam,
                    &mut current_map,
                );
                manager.menu_open = false;
                break;
            }
        }
        return;
    }

    // 'R' returns to the last checkpoint of a race in progress, else restarts the current mode;
    // Delete always restarts the run from its start.
    let restart = keys.just_pressed(KeyCode::Delete);
    if restart || keys.just_pressed(KeyCode::KeyR) {
        let respawn = match (manager.current_trial(), &manager.state) {
            (Some(trial), TrialState::Racing { .. }) if !restart => {
                checkpoint_respawn(trial, &manager.progress)
            }
            _ => None,
        };
        if let Some((at, yaw)) = respawn {
            if let Ok(mut car_inst) = car.single_mut() {
                car_inst.set_spawn(at, yaw);
            }
            if let Ok(mut cam) = chase_cam.single_mut() {
                cam.yaw = yaw;
                cam.snapped = false;
            }
        } else if let Some(idx) = manager.current_trial_idx {
            start_trial_by_index(
                idx,
                &mut manager,
                &mut car,
                &mut chase_cam,
                &mut current_map,
            );
        } else {
            // Free driving reset to current active spawn location
            if let Ok(mut car_inst) = car.single_mut() {
                car_inst.reset_to_spawn();
                if let Ok(mut cam) = chase_cam.single_mut() {
                    cam.yaw = car_inst.spawn_yaw;
                    cam.snapped = false;
                }
            }
        }
    }
}

/// Applies a menu choice by index:
/// - `0..FREE_DRIVES.len()` = Free Driving at that spot
/// - after that = Time Trials
pub fn apply_menu_choice(
    choice: usize,
    manager: &mut TimeTrialManager,
    car: &mut Query<&mut Car>,
    chase_cam: &mut Query<&mut ChaseCam>,
    current_map: &mut ResMut<CurrentMap>,
) {
    if let Some(spot) = FREE_DRIVES.get(choice) {
        manager.current_trial_idx = None;
        manager.free_drive_loc = choice;
        manager.state = TrialState::FreeDriving;
        if current_map.0 != spot.map_id {
            current_map.0 = spot.map_id;
        }
        let yaw = spot.yaw_deg.to_radians();
        if let Ok(mut car_inst) = car.single_mut() {
            car_inst.set_spawn(spot.wow, yaw);
        }
        if let Ok(mut cam) = chase_cam.single_mut() {
            cam.yaw = yaw;
            cam.snapped = false;
        }
        eprintln!(
            "benilla-lambo: switched to Free Driving ({} - {})",
            spot.town, spot.zone
        );
    } else {
        let trial_idx = choice - FREE_DRIVES.len();
        start_trial_by_index(trial_idx, manager, car, chase_cam, current_map);
    }
}

/// The menu rows a page lists, in `apply_menu_choice` indices; `None` for the settings page.
fn menu_range(tab: MenuTab, trials: usize) -> Option<std::ops::Range<usize>> {
    match tab {
        MenuTab::FreeDrive => Some(0..FREE_DRIVES.len()),
        MenuTab::Race => Some(FREE_DRIVES.len()..FREE_DRIVES.len() + trials),
        MenuTab::Settings => None,
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
    manager.progress = RaceProgress::default();
    manager.versus_known.clear();
    manager.grid_slot = None;
    manager.loading = true;

    // Switch map if trial is on another continent
    if current_map.0 != trial.map_id {
        eprintln!(
            "benilla-lambo: switching map from {} to {} for trial '{}'",
            current_map.0, trial.map_id, trial.name
        );
        current_map.0 = trial.map_id;
    }

    // Position car at start, facing towards finish or using the user's finetuned rotation
    let start_coords = [trial.start[0], trial.start[1], trial.start[2] + 40.0];
    let initial_yaw = start_yaw(&trial);

    if let Ok(mut car_inst) = car.single_mut() {
        car_inst.nitro_timer = 0.0;
        car_inst.nitro_cooldown = 0.0;
        car_inst.just_used_nitro = false;
        car_inst.set_spawn(start_coords, initial_yaw);
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
            if car.placed
                && !manager.loading
                && (car.throttle_input.abs() > 0.05 || car.speed() > 1.0)
            {
                manager.state = TrialState::Racing { elapsed: 0.0 };
            }
        }
        TrialState::Racing { elapsed } => {
            let current_time = elapsed + dt;
            manager.state = TrialState::Racing {
                elapsed: current_time,
            };
            let mut progress = std::mem::take(&mut manager.progress);
            let lap_done = pass_gates(&trial, &mut progress, car.pos);

            let mut race_over = false;
            if lap_done {
                let lap_time = current_time - progress.lap_start;
                progress.lap_times.push(lap_time);
                let best_lap = manager.best_laps.get(&trial.name).copied();
                if best_lap.is_none_or(|b| lap_time < b) {
                    manager.best_laps.insert(trial.name.clone(), lap_time);
                }
                if trial.is_race() {
                    eprintln!(
                        "benilla-lambo: '{}' lap {}/{} {:.2}s",
                        trial.name,
                        progress.lap_times.len(),
                        trial.lap_count(),
                        lap_time
                    );
                }
                if progress.lap_times.len() as u32 >= trial.lap_count() {
                    race_over = true;
                } else {
                    progress.next_checkpoint = 0;
                    progress.lap_start = current_time;
                    progress.armed = false;
                }
            }
            manager.progress = progress;

            if race_over {
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

/// The Settings page: what changes here is applied the moment it changes.
fn settings_page(ui: &mut egui::Ui, s: &mut Settings) {
    ui.add_space(8.0);
    let heading = |ui: &mut egui::Ui, text: &str| {
        ui.label(
            egui::RichText::new(text)
                .color(egui::Color32::from_rgb(255, 140, 0))
                .size(13.0)
                .strong(),
        );
    };

    heading(ui, "SOUND");
    let mut percent = (s.volume * 100.0).round();
    ui.add(
        egui::Slider::new(&mut percent, 0.0..=100.0)
            .suffix("%")
            .text("Master volume"),
    );
    s.volume = percent / 100.0;
    ui.checkbox(&mut s.engine_sound, "Engine sound");
    ui.label(
        egui::RichText::new("Untick if the game stutters: the engine is simulated live.")
            .color(egui::Color32::GRAY)
            .size(12.0),
    );

    ui.add_space(12.0);
    heading(ui, "GRAPHICS");
    ui.add(
        egui::Slider::new(&mut s.view_distance, FARCLIP_RANGE)
            .suffix(" yd")
            .text("View distance"),
    );
    ui.label(
        egui::RichText::new("Shorter is smoother on slower machines.")
            .color(egui::Color32::GRAY)
            .size(12.0),
    );
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Resolution");
        for scale in RENDER_SCALES {
            let label = format!("{:.0}%", scale * 100.0);
            if ui
                .selectable_label((s.render_scale - scale).abs() < 0.01, label)
                .clicked()
            {
                s.render_scale = scale;
            }
        }
    });
    ui.label(
        egui::RichText::new(
            "Renders fewer pixels and stretches them to the window: the biggest speed-up.",
        )
        .color(egui::Color32::GRAY)
        .size(12.0),
    );
    ui.add_space(6.0);
    ui.checkbox(&mut s.tire_smoke, "Tyre smoke (drifts and burnouts)");

    ui.add_space(12.0);
    if ui.button("Reset to defaults").clicked() {
        *s = Settings::default();
    }
}

/// The ESC menu's accent colour.
const MENU_ACCENT: egui::Color32 = egui::Color32::from_rgb(255, 140, 0);
/// Highlight for the selected row's title.
const MENU_GOLD: egui::Color32 = egui::Color32::from_rgb(255, 210, 60);

/// A free-driving spawn point.
struct FreeDrive {
    zone: &'static str,
    town: &'static str,
    continent: &'static str,
    map_id: u32,
    wow: [f32; 3],
    yaw_deg: f32,
}

/// The free-driving spots, in menu order (the first `apply_menu_choice` indices).
const FREE_DRIVES: [FreeDrive; 10] = [
    FreeDrive {
        zone: "Redridge Mountains",
        town: "Lakeshire",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: LAKESHIRE_WOW,
        yaw_deg: -90.0,
    },
    FreeDrive {
        zone: "The Barrens",
        town: "Crossroads",
        continent: "Kalimdor",
        map_id: 1,
        wow: CROSSROADS_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Elwynn Forest",
        town: "Goldshire",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: GOLDSHIRE_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Westfall",
        town: "Sentinel Hill",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: SENTINEL_HILL_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Duskwood",
        town: "Darkshire",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: DARKSHIRE_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Hillsbrad Foothills",
        town: "Southshore",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: SOUTHSHORE_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Dun Morogh",
        town: "Kharanos",
        continent: "Eastern Kingdoms",
        map_id: 0,
        wow: KHARANOS_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Durotar",
        town: "Razor Hill",
        continent: "Kalimdor",
        map_id: 1,
        wow: RAZOR_HILL_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Mulgore",
        town: "Bloodhoof Village",
        continent: "Kalimdor",
        map_id: 1,
        wow: BLOODHOOF_WOW,
        yaw_deg: 0.0,
    },
    FreeDrive {
        zone: "Tanaris",
        town: "Gadgetzan",
        continent: "Kalimdor",
        map_id: 1,
        wow: GADGETZAN_WOW,
        yaw_deg: 0.0,
    },
];

/// One entry of the menu's Drive page.
struct MenuRow<'a> {
    title: &'a str,
    subtitle: String,
    versus: bool,
    best: Option<String>,
    active: bool,
    selected: bool,
}

/// Text that does not take the pointer, so a click falls through to the row under it.
fn plain(ui: &mut egui::Ui, text: egui::RichText) {
    ui.add(egui::Label::new(text).selectable(false));
}

/// A small outlined tag, such as "ACTIVE".
fn badge(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .stroke(egui::Stroke::new(1.0_f32, color))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            plain(
                ui,
                egui::RichText::new(text).size(11.0).color(color).strong(),
            );
        });
}

/// A section title, with an optional note on the right.
fn section_heading(ui: &mut egui::Ui, title: &str, note: Option<&str>) {
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        plain(
            ui,
            egui::RichText::new(title)
                .color(MENU_ACCENT)
                .size(12.0)
                .strong(),
        );
        if let Some(note) = note {
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                plain(
                    ui,
                    egui::RichText::new(note)
                        .color(egui::Color32::GRAY)
                        .size(11.0),
                );
            });
        }
    });
    ui.add_space(2.0);
}

/// A key cap and what it does, for the menu's footer.
fn key_hint(ui: &mut egui::Ui, key: &str, action: &str) {
    egui::Frame::new()
        .fill(egui::Color32::from_white_alpha(14))
        .stroke(egui::Stroke::new(
            1.0_f32,
            egui::Color32::from_white_alpha(40),
        ))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| {
            plain(
                ui,
                egui::RichText::new(key)
                    .monospace()
                    .size(11.0)
                    .color(egui::Color32::LIGHT_GRAY),
            );
        });
    plain(
        ui,
        egui::RichText::new(action)
            .size(12.0)
            .color(egui::Color32::GRAY),
    );
    ui.add_space(6.0);
}

/// Draws one Drive-page row as a card and returns its click response. `idx` is its menu index:
/// the first nine carry their number key.
fn menu_row(ui: &mut egui::Ui, idx: usize, row: &MenuRow) -> egui::Response {
    let (fill, stroke) = if row.selected {
        (
            egui::Color32::from_rgb(58, 38, 8),
            egui::Stroke::new(1.5_f32, MENU_ACCENT),
        )
    } else {
        (
            egui::Color32::from_white_alpha(6),
            egui::Stroke::new(1.0_f32, egui::Color32::from_white_alpha(18)),
        )
    };
    let card = egui::Frame::new()
        .fill(fill)
        .stroke(stroke)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.set_min_height(34.0);

                // The number key, 1-9.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(22.0, 22.0), egui::Sense::hover());
                if idx < 9 {
                    ui.painter().rect_filled(
                        rect,
                        4.0,
                        if row.selected {
                            MENU_ACCENT
                        } else {
                            egui::Color32::from_white_alpha(16)
                        },
                    );
                    ui.painter().text(
                        rect.center(),
                        egui::Align2::CENTER_CENTER,
                        (idx + 1).to_string(),
                        egui::FontId::monospace(13.0),
                        if row.selected {
                            egui::Color32::BLACK
                        } else {
                            egui::Color32::LIGHT_GRAY
                        },
                    );
                }
                ui.add_space(4.0);

                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.horizontal(|ui| {
                        plain(
                            ui,
                            egui::RichText::new(row.title).size(15.0).strong().color(
                                if row.selected {
                                    MENU_GOLD
                                } else {
                                    egui::Color32::WHITE
                                },
                            ),
                        );
                        if row.versus {
                            badge(ui, "VERSUS", egui::Color32::from_rgb(120, 200, 255));
                        }
                    });
                    plain(
                        ui,
                        egui::RichText::new(&row.subtitle)
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                    );
                });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if row.active {
                        badge(ui, "ACTIVE", egui::Color32::from_rgb(110, 220, 120));
                    }
                    if let Some(best) = &row.best {
                        plain(
                            ui,
                            egui::RichText::new(format!("★ {best}"))
                                .monospace()
                                .size(13.0)
                                .color(MENU_GOLD),
                        );
                    }
                });
            });
        });
    ui.interact(
        card.response.rect,
        ui.id().with(("esc_menu_row", idx)),
        egui::Sense::click(),
    )
    .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Renders the in-game Time Trial UI: the ESC menu modal and the live race HUD.
pub fn render_trial_ui(
    mut contexts: EguiContexts,
    mut manager: ResMut<TimeTrialManager>,
    mut car: Query<&mut Car>,
    mut chase_cam: Query<&mut ChaseCam>,
    mut current_map: ResMut<CurrentMap>,
    mut settings: ResMut<Settings>,
    mut tab: ResMut<MenuTab>,
    lobby: Res<VersusLobby>,
    preload: Res<crate::preload::CoursePreload>,
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
            c.burnout,
            c.brake_input,
        )
    });

    // 1. IN-GAME HUD (Top bar when racing, waiting, or finished)
    if let Some(trial) = manager.current_trial().cloned() {
        let target_bevy = wow_to_bevy(manager.target_gate().unwrap_or(trial.end));
        let heading_to_checkpoint = manager.progress.next_checkpoint < trial.checkpoints.len();
        let car_pos = car_telemetry.map(|t| t.7).unwrap_or(Vec3::ZERO);
        let dist_to_finish =
            Vec2::new(car_pos.x - target_bevy.x, car_pos.z - target_bevy.z).length();
        let race_label = manager.race_label();
        let lap_times = manager.progress.lap_times.clone();
        let best_lap = manager.best_laps.get(&trial.name).copied();
        let drivers = lobby.rivals.len() + 1;

        if manager.loading {
            let (ready, total) = preload.pending().unwrap_or((0, 0));
            egui::Area::new(egui::Id::new("course_loading"))
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .interactable(false)
                .show(ctx, |ui| {
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_black_alpha(220))
                        .stroke(egui::Stroke::new(
                            1.5_f32,
                            egui::Color32::from_rgb(255, 170, 0),
                        ))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(32, 20))
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(
                                    egui::RichText::new("LOADING MAP")
                                        .color(egui::Color32::from_rgb(255, 230, 80))
                                        .size(28.0)
                                        .strong(),
                                );
                                ui.label(
                                    egui::RichText::new(if total > 0 {
                                        format!("{ready}/{total}")
                                    } else {
                                        "preparing the course…".to_string()
                                    })
                                    .color(egui::Color32::LIGHT_GRAY)
                                    .size(15.0),
                                );
                            });
                        });
                });
        }

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

                            if !race_label.is_empty() {
                                ui.label(
                                    egui::RichText::new(&race_label)
                                        .color(egui::Color32::from_rgb(120, 220, 255))
                                        .size(16.0)
                                        .strong(),
                                );
                            }

                            if trial.versus && lobby.online {
                                let text = if manager.frozen() {
                                    format!(
                                        "VERSUS — {} driver{} on the grid",
                                        drivers,
                                        if drivers == 1 { "" } else { "s" }
                                    )
                                } else {
                                    "VERSUS".to_string()
                                };
                                ui.label(
                                    egui::RichText::new(text)
                                    .color(egui::Color32::from_rgb(255, 90, 60))
                                    .size(15.0)
                                    .strong(),
                                );
                            }

                            match manager.state {
                                TrialState::Countdown { remaining } => {
                                    ui.label(
                                        egui::RichText::new(format!("{}", remaining.ceil() as u32))
                                            .color(egui::Color32::from_rgb(255, 230, 80))
                                            .size(52.0)
                                            .monospace()
                                            .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new(
                                            "Cars are held until the count ends — a driver joining restarts it",
                                        )
                                        .color(egui::Color32::LIGHT_GRAY)
                                        .size(13.0),
                                    );
                                }
                                TrialState::WaitingToStart if manager.loading => {
                                    ui.label(
                                        egui::RichText::new("LOADING MAP — the car is held until it is ready")
                                            .color(egui::Color32::from_rgb(255, 230, 80))
                                            .size(18.0)
                                            .strong(),
                                    );
                                }
                                TrialState::WaitingToStart => {
                                    if trial.versus {
                                        ui.label(
                                            egui::RichText::new(
                                                "Versus course — drive with friends to race others",
                                            )
                                            .color(egui::Color32::from_rgb(255, 120, 90))
                                            .size(13.0),
                                        );
                                    }
                                    ui.label(
                                        egui::RichText::new(
                                            "READY — PRESS 'W' / ACCELERATE TO START",
                                        )
                                        .color(egui::Color32::from_rgb(100, 255, 100))
                                        .size(20.0)
                                        .strong(),
                                    );
                                    ui.add_space(2.0);
                                    ui.label(
                                        egui::RichText::new(
                                            "⚡ Pure Skill: Nitro is disabled during Time Trials",
                                        )
                                        .color(egui::Color32::from_rgb(220, 180, 100))
                                        .size(12.0),
                                    );
                                }
                                TrialState::Racing { elapsed } => {
                                    ui.label(
                                        egui::RichText::new(format_time(elapsed))
                                            .color(egui::Color32::from_rgb(255, 230, 80))
                                            .size(28.0)
                                            .monospace()
                                            .strong(),
                                    );
                                    if !race_label.is_empty() {
                                        let lap_now = elapsed - manager.progress.lap_start;
                                        let mut line = format!("Lap {}", format_time(lap_now));
                                        if let Some(last) = lap_times.last() {
                                            line.push_str(&format!(
                                                "  ·  Last {}",
                                                format_time(*last)
                                            ));
                                        }
                                        if let Some(best) = best_lap {
                                            line.push_str(&format!(
                                                "  ·  Best {}",
                                                format_time(best)
                                            ));
                                        }
                                        ui.label(
                                            egui::RichText::new(line)
                                                .color(egui::Color32::LIGHT_GRAY)
                                                .size(13.0)
                                                .monospace(),
                                        );
                                    }
                                }
                                TrialState::Finished {
                                    final_time,
                                    is_best,
                                } => {
                                    let time_str = format_time(final_time);
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
                                    if lap_times.len() > 1 {
                                        for (i, lap) in lap_times.iter().enumerate() {
                                            ui.label(
                                                egui::RichText::new(format!(
                                                    "Lap {}  {}",
                                                    i + 1,
                                                    format_time(*lap)
                                                ))
                                                .color(egui::Color32::LIGHT_GRAY)
                                                .size(13.0)
                                                .monospace(),
                                            );
                                        }
                                    }
                                    ui.label(
                                        egui::RichText::new("Press 'R' or DEL to retry, or ESC for menu")
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
                                    "{}: {:.0} yd ({:.2} km)",
                                    if heading_to_checkpoint {
                                        "Checkpoint"
                                    } else {
                                        "Finish"
                                    },
                                    dist_to_finish,
                                    dist_km
                                ))
                                .color(egui::Color32::from_rgb(200, 200, 200))
                                .size(14.0),
                            );

                            if let Some((ready, total)) = preload.pending() {
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Loading course {ready}/{total}"
                                    ))
                                    .color(egui::Color32::GRAY)
                                    .size(12.0),
                                );
                            }

                            if !matches!(manager.state, TrialState::Finished { .. }) {
                                ui.add_space(2.0);
                                ui.label(
                                    egui::RichText::new("R to respawn  ·  DEL to restart run")
                                        .color(egui::Color32::GRAY)
                                        .size(11.0),
                                );
                            }
                        });
                    });
            });
    }

    // 2. ESC MENU MODAL
    if manager.menu_open {
        let mut chosen_idx: Option<usize> = None;
        let mut hovered_idx: Option<usize> = None;
        let mut close = false;
        let selected = manager.selected_menu_idx;
        let mut edit = settings.clone();
        let mut new_tab = *tab;
        let mut tab_selection: Option<usize> = None;
        let screen = ctx.content_rect();

        // Dim the world behind the menu so it reads as paused.
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Background,
            egui::Id::new("esc_menu_backdrop"),
        ))
        .rect_filled(screen, 0.0, egui::Color32::from_black_alpha(140));

        let now_driving = match manager.current_trial() {
            Some(trial) => trial.name.clone(),
            None => {
                let spot = &FREE_DRIVES[manager.free_drive_loc.min(FREE_DRIVES.len() - 1)];
                format!("Free Driving — {}, {}", spot.zone, spot.town)
            }
        };

        egui::Window::new("TIME TRIAL MENU")
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .resizable(false)
            .collapsible(false)
            .title_bar(false)
            .frame(
                egui::Frame::window(&ctx.style())
                    .fill(egui::Color32::from_rgb(16, 14, 12))
                    .stroke(egui::Stroke::new(1.5_f32, MENU_ACCENT))
                    .corner_radius(12.0)
                    .inner_margin(egui::Margin::same(20)),
            )
            .show(ctx, |ui| {
                ui.set_width(560.0);

                // Header: title, what is running now, and the page tabs.
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        plain(
                            ui,
                            egui::RichText::new("BENILLA LAMBO")
                                .color(MENU_ACCENT)
                                .size(24.0)
                                .strong(),
                        );
                        plain(
                            ui,
                            egui::RichText::new(format!("Now: {now_driving}"))
                                .color(egui::Color32::LIGHT_GRAY)
                                .size(13.0),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        for (page, label) in [
                            (MenuTab::Settings, "Settings"),
                            (MenuTab::Race, "Racing"),
                            (MenuTab::FreeDrive, "Free Driving"),
                        ] {
                            let on = new_tab == page;
                            let button = egui::Button::new(
                                egui::RichText::new(label).size(15.0).strong().color(if on {
                                    egui::Color32::BLACK
                                } else {
                                    egui::Color32::LIGHT_GRAY
                                }),
                            )
                            .fill(if on {
                                MENU_ACCENT
                            } else {
                                egui::Color32::from_white_alpha(10)
                            })
                            .corner_radius(6.0)
                            .min_size(egui::vec2(92.0, 30.0));
                            if ui.add(button).clicked() {
                                new_tab = page;
                                if let Some(range) = menu_range(page, manager.trials.len()) {
                                    if !range.contains(&selected) {
                                        tab_selection = Some(range.start);
                                    }
                                }
                            }
                        }
                    });
                });
                ui.add_space(10.0);
                ui.separator();

                if new_tab == MenuTab::Settings {
                    settings_page(ui, &mut edit);
                } else {
                    let free_page = new_tab == MenuTab::FreeDrive;
                    // Scroll the list on a short window; follow the keyboard selection.
                    let last_scrolled = egui::Id::new("esc_menu_scrolled_to");
                    let scrolled_to = ui.ctx().data(|d| d.get_temp::<usize>(last_scrolled));
                    let pointer_moving = ui.ctx().input(|i| i.pointer.is_moving());

                    egui::ScrollArea::vertical()
                        .max_height((screen.height() - 220.0).max(160.0))
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            let mut row_ui = |ui: &mut egui::Ui, idx: usize, row: MenuRow| {
                                let response = menu_row(ui, idx, &row);
                                if response.hovered() && pointer_moving {
                                    hovered_idx = Some(idx);
                                }
                                if response.clicked() {
                                    chosen_idx = Some(idx);
                                }
                                if row.selected && scrolled_to != Some(idx) {
                                    response.scroll_to_me(None);
                                    ui.ctx().data_mut(|d| d.insert_temp(last_scrolled, idx));
                                }
                                ui.add_space(4.0);
                            };

                            if free_page {
                                section_heading(ui, "FREE DRIVING", None);
                                for (idx, spot) in FREE_DRIVES.iter().enumerate() {
                                    row_ui(
                                        ui,
                                        idx,
                                        MenuRow {
                                            title: spot.zone,
                                            subtitle: format!(
                                                "{} • {}",
                                                spot.town, spot.continent
                                            ),
                                            versus: false,
                                            best: None,
                                            active: manager.current_trial_idx.is_none()
                                                && manager.free_drive_loc == idx,
                                            selected: selected == idx,
                                        },
                                    );
                                }
                                return;
                            }

                            section_heading(ui, "TIME TRIALS & RACES", Some("Nitro off"));
                            for (i, trial) in manager.trials.iter().enumerate() {
                                let idx = i + FREE_DRIVES.len();
                                let continent = match trial.map_id {
                                    0 => "Eastern Kingdoms".to_string(),
                                    1 => "Kalimdor".to_string(),
                                    other => format!("Map {other}"),
                                };
                                let shape = if trial.is_race() {
                                    let laps = trial.lap_count();
                                    let gates = trial.checkpoints.len();
                                    format!(
                                        "{laps} lap{} • {gates} checkpoint{}",
                                        if laps == 1 { "" } else { "s" },
                                        if gates == 1 { "" } else { "s" },
                                    )
                                } else {
                                    "Sprint".to_string()
                                };
                                row_ui(
                                    ui,
                                    idx,
                                    MenuRow {
                                        title: &trial.name,
                                        subtitle: format!("{continent} • {shape}"),
                                        versus: trial.versus,
                                        best: manager
                                            .best_times
                                            .get(&trial.name)
                                            .map(|b| format_time(*b)),
                                        active: manager.current_trial_idx == Some(i),
                                        selected: selected == idx,
                                    },
                                );
                            }
                        });
                }

                ui.add_space(10.0);
                ui.separator();
                ui.add_space(6.0);

                // Footer: the keys, and a mouse way out.
                ui.horizontal(|ui| {
                    let hints: &[(&str, &str)] = if new_tab == MenuTab::Settings {
                        &[("Tab", "Free Driving")]
                    } else {
                        &[
                            ("↑↓", "Select"),
                            ("Enter", "Drive"),
                            ("1-9", "Quick pick"),
                            ("Tab", "Next page"),
                        ]
                    };
                    for (key, action) in hints {
                        key_hint(ui, key, action);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new("Resume  (Esc)")
                                        .size(14.0)
                                        .color(egui::Color32::LIGHT_GRAY),
                                )
                                .corner_radius(6.0)
                                .min_size(egui::vec2(0.0, 28.0)),
                            )
                            .clicked()
                        {
                            close = true;
                        }
                    });
                });
            });

        if close {
            manager.menu_open = false;
        }
        if let Some(idx) = tab_selection {
            manager.selected_menu_idx = idx;
        }
        if let Some(idx) = hovered_idx {
            manager.selected_menu_idx = idx;
            // The mouse picked it: no need to scroll the list to it.
            ctx.data_mut(|d| d.insert_temp(egui::Id::new("esc_menu_scrolled_to"), idx));
        }
        if new_tab != *tab {
            *tab = new_tab;
        }
        if edit != *settings {
            *settings = edit;
        }
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
        burnout,
        brake_input,
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
                            if manager.current_trial().is_some_and(|t| t.versus) && lobby.online {
                                ui.label(
                                    egui::RichText::new("VERSUS")
                                        .color(egui::Color32::from_rgb(255, 90, 60))
                                        .size(11.0)
                                        .strong(),
                                );
                            } else if manager.current_trial_idx.is_some() {
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
                                    if !grounded && placed {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(100, 200, 255),
                                            "AIR",
                                        );
                                    } else if burnout {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 120, 20),
                                            "BURNOUT",
                                        );
                                    } else if handbrake {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 60, 60),
                                            "HANDBRAKE",
                                        );
                                    } else if brake_input > 0.0 && speed_kmh > 5 {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 60, 60),
                                            "BRAKE",
                                        );
                                    } else if lateral_speed > 3.2 {
                                        ui.colored_label(
                                            egui::Color32::from_rgb(255, 165, 0),
                                            "DRIFT",
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
                        let in_trial = manager.current_trial_idx.is_some();
                        let (nitro_label, nitro_color, nitro_fill_color, nitro_ratio) = if in_trial
                        {
                            (
                                "🚫 NITRO DISABLED (TIME TRIAL)".to_string(),
                                egui::Color32::from_rgb(200, 80, 80),
                                egui::Color32::from_rgb(90, 30, 30),
                                0.0_f32,
                            )
                        } else if is_boosting {
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

    // 4. DEBUG TELEMETRY OVERLAY (Top right, toggled with 'G')
    if manager.debug_open {
        if let Some(c) = car.iter().next() {
            let wow_pos = bevy_to_wow(c.pos);
            let yaw_deg = c.yaw.to_degrees().rem_euclid(360.0);
            let map_id = current_map.0;
            let map_name = match map_id {
                0 => "Eastern Kingdoms (0)".to_string(),
                1 => "Kalimdor (1)".to_string(),
                other => format!("Map {other}"),
            };

            egui::Area::new(egui::Id::new("debug_telemetry_menu"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-20.0, 20.0))
                .show(ctx, |ui| {
                    egui::Frame::window(&ctx.style())
                        .fill(egui::Color32::from_black_alpha(225))
                        .stroke(egui::Stroke::new(1.5, egui::Color32::from_rgb(0, 220, 255)))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::symmetric(16, 12))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(
                                    egui::RichText::new("🛠 DEBUG TELEMETRY")
                                        .color(egui::Color32::from_rgb(0, 220, 255))
                                        .size(13.0)
                                        .strong(),
                                );
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        ui.label(
                                            egui::RichText::new("[G] hide")
                                                .color(egui::Color32::GRAY)
                                                .size(11.0),
                                        );
                                    },
                                );
                            });
                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(6.0);

                            egui::Grid::new("debug_grid")
                                .num_columns(2)
                                .spacing([12.0, 6.0])
                                .show(ui, |ui| {
                                    ui.label(
                                        egui::RichText::new("Map ID:")
                                            .color(egui::Color32::LIGHT_GRAY)
                                            .strong(),
                                    );
                                    ui.label(
                                        egui::RichText::new(map_name)
                                            .color(egui::Color32::from_rgb(255, 215, 0))
                                            .strong(),
                                    );
                                    ui.end_row();

                                    ui.label(
                                        egui::RichText::new("Coordinates:")
                                            .color(egui::Color32::LIGHT_GRAY)
                                            .strong(),
                                    );
                                    let coords_str = format!(
                                        "[{:.2}, {:.2}, {:.2}]",
                                        wow_pos[0], wow_pos[1], wow_pos[2]
                                    );
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(&coords_str)
                                                .color(egui::Color32::from_rgb(100, 255, 140))
                                                .monospace(),
                                        );
                                        // The `[x, y, z]` form `time_trials.json` takes.
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(coords_str.clone());
                                        }
                                    });
                                    ui.end_row();

                                    ui.label(
                                        egui::RichText::new("Rotation:")
                                            .color(egui::Color32::LIGHT_GRAY)
                                            .strong(),
                                    );
                                    let rot_str = format!("{:.1}° ({:.2} rad)", yaw_deg, c.yaw);
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            egui::RichText::new(rot_str)
                                                .color(egui::Color32::from_rgb(255, 160, 50))
                                                .monospace(),
                                        );
                                        // Degrees, the unit of a course's `rotation`.
                                        if ui.small_button("Copy").clicked() {
                                            ui.ctx().copy_text(format!("{yaw_deg:.1}"));
                                        }
                                    });
                                    ui.end_row();
                                });
                        });
                });
        }
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
        // A point-to-point race with checkpoints.
        assert_eq!(trials[0].name, "Tauren's Track");
        assert_eq!(trials[0].map_id, 1);
        assert_eq!(trials[0].start, [-2916.76, -254.16, 53.46]);
        assert_eq!(trials[0].end, [-2351.45, -1672.22, 84.56]);
        assert_eq!(trials[0].rotation, Some(181.9));
        assert_eq!(trials[0].checkpoints.len(), 6);
        assert!(!trials[0].circuit && trials[0].lap_count() == 1);
        assert_eq!(trials[1].name, "Foothills Run");
        assert_eq!(trials[1].map_id, 0);
        assert_eq!(trials[1].checkpoints.len(), 6);
        assert!(!trials[1].circuit && trials[1].lap_count() == 1);
        assert_eq!(trials[2].name, "Dusty Dunes");
        assert_eq!(trials[2].map_id, 1);
        assert_eq!(trials[2].checkpoints.len(), 9);
        assert!(!trials[2].circuit && trials[2].lap_count() == 1);
        // A two-lap circuit finishing at its start.
        assert_eq!(trials[3].name, "Mirage Raceway");
        assert!(trials[3].circuit && trials[3].end == trials[3].start);
        assert_eq!(trials[3].lap_count(), 2);
        assert_eq!(trials[3].checkpoints.len(), 6);
        // The first six are versus courses; the rest are not.
        assert!(trials[..6].iter().all(|t| t.versus));
        assert!(trials[6..].iter().all(|t| !t.versus));
        assert_eq!(trials[4].name, "Redridge Highway");
        assert_eq!(trials[4].checkpoints.len(), 5);
        // A two-lap circuit finishing at its start.
        assert_eq!(trials[5].name, "Around the Forge");
        assert!(trials[5].circuit && trials[5].lap_count() == 2);
        assert_eq!(trials[5].checkpoints.len(), 10);
    }

    /// A three-checkpoint circuit: the line is an arch (index 3) until the last lap, then the
    /// checkered finish.
    #[test]
    fn the_finish_stands_only_on_the_last_lap() {
        let shown = |index, next, last_lap| gate_shown(index, next, 3, last_lap, true);
        // Early lap, heading for checkpoint 2: it and the lap-line arch after it show, no finish.
        assert!(shown(Some(2), 2, false) && shown(Some(3), 2, false));
        assert!(!shown(None, 2, false));
        // Heading for the line: the arch, and the next lap's first gate behind it.
        assert!(shown(Some(3), 3, false) && shown(Some(0), 3, false));
        assert!(!shown(None, 3, false));
        // Last lap: the checkered finish, never the arch.
        assert!(shown(None, 0, true) && shown(None, 3, true));
        assert!(!shown(Some(3), 2, true) && !shown(Some(3), 3, true));
        assert!(!shown(Some(0), 3, true));
        // Early in a lap the arch is far off and hidden.
        assert!(!shown(Some(3), 0, false));
        // A circuit with no checkpoints: the arch is always the gate driven at.
        assert!(gate_shown(Some(0), 0, 0, false, true));
        assert!(!gate_shown(None, 0, 0, false, true));
    }

    fn parse(json: &str) -> TimeTrial {
        let defs: Vec<TimeTrialDef> = serde_json::from_str(json).unwrap();
        defs.into_iter().next().unwrap().into()
    }

    #[test]
    fn the_finish_counts_only_after_every_checkpoint_in_order() {
        let race = parse(
            r#"[{"name": "T", "map": 0, "start": [0,0,0],
                 "checkpoints": [[100,0,0], [200,0,0]], "end": [300,0,0]}]"#,
        );
        let at = |x: f32| wow_to_bevy([x, 0.0, 0.0]);
        let mut progress = RaceProgress::default();
        pass_gates(&race, &mut progress, at(50.0));
        // Straight to the finish: no finish.
        assert!(!pass_gates(&race, &mut progress, at(300.0)));
        // The second checkpoint before the first is not taken.
        pass_gates(&race, &mut progress, at(200.0));
        assert_eq!(progress.next_checkpoint, 0);
        pass_gates(&race, &mut progress, at(100.0));
        pass_gates(&race, &mut progress, at(200.0));
        assert_eq!(progress.next_checkpoint, 2);
        assert!(pass_gates(&race, &mut progress, at(300.0)));
    }

    #[test]
    fn respawn_returns_to_the_last_checkpoint_facing_the_next_gate() {
        let race = parse(
            r#"[{"name": "T", "map": 0, "start": [0,0,0],
                 "checkpoints": [[100,0,0], [200,0,0]], "end": [300,0,0]}]"#,
        );
        let mut progress = RaceProgress::default();
        // Before the first checkpoint R restarts.
        assert!(checkpoint_respawn(&race, &progress).is_none());
        progress.next_checkpoint = 1;
        let (at, yaw) = checkpoint_respawn(&race, &progress).unwrap();
        assert_eq!(at, [100.0, 0.0, 40.0]);
        let d = wow_to_bevy([200.0, 0.0, 0.0]) - wow_to_bevy([100.0, 0.0, 0.0]);
        assert!((yaw - (-d.x).atan2(-d.z)).abs() < 1e-5);
        // After the last checkpoint it faces the finish.
        progress.next_checkpoint = 2;
        let (at, _) = checkpoint_respawn(&race, &progress).unwrap();
        assert_eq!(at, [200.0, 0.0, 40.0]);
    }

    #[test]
    fn grid_places_line_up_behind_the_start_facing_one_way() {
        let race = parse(
            r#"[{"name": "G", "map": 0, "start": [0,0,0], "rotation": 90, "end": [500,0,0]}]"#,
        );
        let places: Vec<_> = (0..4).map(|slot| grid_spawn(&race, slot)).collect();
        let start = wow_to_bevy(race.start);
        let yaw = 90f32.to_radians();
        let fwd = Vec3::new(-yaw.sin(), 0.0, -yaw.cos());
        for (slot, (at, heading)) in places.iter().enumerate() {
            assert!((heading - yaw).abs() < 1e-5);
            let back = (start - wow_to_bevy(*at)).dot(fwd);
            assert!((back - slot as f32 * GRID_BACK).abs() < 1e-3, "slot {slot}");
            // Dropped from above the start.
            assert_eq!(at[2], 40.0);
        }
        for a in 0..places.len() {
            for b in a + 1..places.len() {
                let d = wow_to_bevy(places[a].0) - wow_to_bevy(places[b].0);
                assert!(Vec2::new(d.x, d.z).length() > 4.0, "{a} and {b} overlap");
            }
        }
    }

    #[test]
    fn the_example_courses_parse() {
        let defs: Vec<TimeTrialDef> =
            serde_json::from_str(include_str!("../assets/time_trials.example.json"))
                .expect("parse time_trials.example.json");
        let trials: Vec<TimeTrial> = defs.into_iter().map(Into::into).collect();
        assert_eq!(trials.len(), 3);
        assert!(!trials[0].is_race());
        assert!(!trials[1].circuit && trials[1].checkpoints.len() == 2);
        assert!(trials[2].circuit && trials[2].lap_count() == 3);
    }

    #[test]
    fn test_parse_circuit_race() {
        let race = parse(
            r#"[{
                "name": "Loop", "map": 0,
                "start": [0.0, 0.0, 0.0],
                "checkpoints": [[100.0, 0.0, 0.0], {"x": 100.0, "y": 100.0, "z": 0.0}],
                "loop": true, "laps": 3, "gate_radius": 25.0
            }]"#,
        );
        assert!(race.circuit);
        assert_eq!(race.end, race.start);
        assert_eq!(race.lap_count(), 3);
        assert_eq!(race.checkpoints.len(), 2);
        assert_eq!(race.checkpoints[1], [100.0, 100.0, 0.0]);
        assert_eq!(race.checkpoint_radius, 25.0);
        assert!(race.is_race());
    }

    #[test]
    fn test_parse_point_to_point_race_and_lap_rules() {
        // Checkpoints with an explicit finish: one lap, however many are asked for.
        let race = parse(
            r#"[{"name": "A", "map_id": 1, "start": [0,0,0], "end": [9,9,9],
                 "checkpoints": [[5,5,5]], "laps": 4}]"#,
        );
        assert!(!race.circuit);
        assert_eq!(race.end, [9.0, 9.0, 9.0]);
        assert_eq!(race.lap_count(), 1);
        assert_eq!(race.checkpoint_radius, DEFAULT_GATE_RADIUS);
        // Leaving out `end` makes a circuit even without `loop`.
        assert!(parse(r#"[{"name": "B", "map_id": 0, "start": [0,0,0], "laps": 2}]"#).circuit);
    }

    #[test]
    fn test_gate_needs_height_and_radius() {
        let gate = [0.0, 0.0, 0.0];
        let at = wow_to_bevy(gate);
        assert!(in_gate(at, gate, 16.0));
        assert!(in_gate(at + Vec3::new(15.0, 40.0, 0.0), gate, 16.0));
        assert!(!in_gate(at + Vec3::new(17.0, 0.0, 0.0), gate, 16.0));
        assert!(!in_gate(at + Vec3::new(0.0, 50.0, 0.0), gate, 16.0));
    }

    #[test]
    fn test_race_label() {
        let mut m = TimeTrialManager {
            trials: vec![parse(
                r#"[{"name": "L", "map_id": 0, "start": [0,0,0],
                     "checkpoints": [[1,1,1],[2,2,2]], "laps": 3}]"#,
            )],
            current_trial_idx: Some(0),
            ..TimeTrialManager::default()
        };
        assert_eq!(m.race_label(), "Lap 1/3 · CP 0/2");
        m.progress.next_checkpoint = 2;
        m.progress.lap_times = vec![30.0];
        assert_eq!(m.race_label(), "Lap 2/3 · CP 2/2");
        assert_eq!(m.target_gate(), Some([0.0, 0.0, 0.0]));
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
