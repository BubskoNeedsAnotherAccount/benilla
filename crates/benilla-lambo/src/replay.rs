//! Replays. Every finished run is kept, and the finish panel saves it to a file on the player's
//! computer with their name in it; the ESC menu's Replays page opens such a file and plays it back
//! on the local car. A course's `goldMedalReplay` is the run to beat for its gold medal: it races
//! alongside as a translucent ghost, which H shows or hides.
//!
//! A replay file is JSON: who drove, the course, the time, and the car's pose 20 times a second
//! (`Frame`). Playback eases between frames and jumps across a respawn.

use std::collections::{HashMap, HashSet};

use bevy::ecs::system::SystemParam;
use bevy::prelude::*;
use bevy_egui::{egui, EguiContexts};
use serde::{Deserialize, Serialize};

use benilla_assets::coords::bevy_to_wow;
use benilla_world::view::WorldCamera;
use benilla_world::world_map::CurrentMap;

use crate::camera::ChaseCam;
use crate::car::{spawn_model, wheel_rotation, Car, CarAssets, Wheels};
use crate::model::WHEEL_RADIUS;
use crate::settings::Settings;
use crate::trial::{
    advance_race, format_time, RaceProgress, TimeTrial, TimeTrialManager, TrialState,
};

/// The `format` every replay file carries.
pub const REPLAY_FORMAT: &str = "benilla-lambo-replay";
/// The newest file layout this build reads and the one it writes.
pub const REPLAY_VERSION: u32 = 1;
/// A replay file's extension.
pub const REPLAY_EXT: &str = "lambo-replay";
/// Poses recorded per second of race time.
const SAMPLE_HZ: f32 = 20.0;
/// Two frames this far apart, yards, are a respawn: playback jumps rather than slides.
const SNAP: f32 = 40.0;
/// Shows or hides the gold medal ghost (`ghost_hint` says so at every race start).
pub const GHOST_KEY: KeyCode = KeyCode::KeyH;
/// Seconds into a race the hint stays up, mid-screen.
pub const GHOST_HINT_SECS: f32 = 4.0;
/// The longest player name a replay keeps.
pub const NAME_MAX: usize = 24;
/// Playback speeds, slowest first.
const SPEEDS: [f32; 6] = [0.25, 0.5, 1.0, 2.0, 4.0, 8.0];
/// Seconds the arrow keys skip in a replay.
const SKIP: f32 = 5.0;
/// The gold colour of the medal, the ghost's tag and its HUD lines.
pub const GOLD: egui::Color32 = egui::Color32::from_rgb(255, 205, 50);

/// The car at one moment: race time, position (Bevy space), yaw, ground normal, velocity, steering
/// angle and throttle. Written as a JSON array.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
pub struct Frame(
    pub f32,
    pub [f32; 3],
    pub f32,
    pub [f32; 3],
    pub [f32; 3],
    pub f32,
    pub f32,
);

/// `x` to `places` decimals, so a file holds no noise digits.
fn round(x: f32, places: i32) -> f32 {
    let k = 10f32.powi(places);
    (x * k).round() / k
}

fn round3(v: Vec3, places: i32) -> [f32; 3] {
    [round(v.x, places), round(v.y, places), round(v.z, places)]
}

impl Frame {
    /// The car's pose at race time `t`.
    pub fn of(car: &Car, t: f32) -> Self {
        Self(
            round(t, 3),
            round3(car.pos, 3),
            round(car.yaw, 4),
            round3(car.up, 4),
            round3(car.vel, 2),
            round(car.steer, 3),
            round(car.throttle_input, 2),
        )
    }

    fn finite(&self) -> bool {
        [self.0, self.2, self.5, self.6]
            .into_iter()
            .chain(self.1)
            .chain(self.3)
            .chain(self.4)
            .all(f32::is_finite)
    }

    fn pose(&self) -> Pose {
        Pose {
            pos: Vec3::from(self.1),
            yaw: self.2,
            up: Vec3::from(self.3).normalize_or(Vec3::Y),
            vel: Vec3::from(self.4),
            steer: self.5,
            throttle: self.6,
        }
    }
}

/// Where a replayed car is, between two frames.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub pos: Vec3,
    pub yaw: f32,
    pub up: Vec3,
    pub vel: Vec3,
    pub steer: f32,
    pub throttle: f32,
}

/// One recorded run, as a replay file holds it.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Replay {
    pub format: String,
    pub version: u32,
    /// Who drove it.
    pub player: String,
    /// The course's name, as `time_trials.json` has it.
    pub course: String,
    pub map_id: u32,
    /// The finishing time, seconds.
    pub time: f32,
    #[serde(default)]
    pub lap_times: Vec<f32>,
    pub frames: Vec<Frame>,
}

impl Replay {
    /// Reads a replay file, refusing one that is not a replay or is damaged.
    pub fn parse(text: &str) -> Result<Self, String> {
        let r: Replay =
            serde_json::from_str(text).map_err(|e| format!("not a replay file ({e})"))?;
        if r.format != REPLAY_FORMAT {
            return Err("not a benilla lambo replay".into());
        }
        if r.version > REPLAY_VERSION {
            return Err(format!(
                "made by a newer version of the game (format {})",
                r.version
            ));
        }
        if r.frames.is_empty() || !r.time.is_finite() || r.time <= 0.0 {
            return Err("the replay has no run in it".into());
        }
        if !r.frames.iter().all(Frame::finite) || r.frames.windows(2).any(|w| w[1].0 < w[0].0) {
            return Err("the replay's frames are damaged".into());
        }
        Ok(r)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Seconds of driving the frames cover.
    pub fn duration(&self) -> f32 {
        self.frames.last().map_or(0.0, |f| f.0)
    }

    /// `Taurens-Track_01-03.21_name.lambo-replay`.
    pub fn file_name(&self) -> String {
        let time = format_time(self.time).replace(':', "-");
        let player = slug(&self.player);
        let player = if player.is_empty() {
            "driver".into()
        } else {
            player
        };
        format!("{}_{time}_{player}.{REPLAY_EXT}", slug(&self.course))
    }

    /// The car at race time `t`, eased between the frames either side; across a respawn it holds the
    /// frame before until the one after.
    pub fn pose_at(&self, t: f32) -> Option<Pose> {
        let f = &self.frames;
        let i = f.partition_point(|fr| fr.0 <= t);
        if i == 0 {
            return f.first().map(Frame::pose);
        }
        if i >= f.len() {
            return f.last().map(Frame::pose);
        }
        let (a, b) = (f[i - 1].pose(), f[i].pose());
        if a.pos.distance(b.pos) > SNAP {
            return Some(a);
        }
        let span = f[i].0 - f[i - 1].0;
        let k = if span > 0.0 {
            ((t - f[i - 1].0) / span).clamp(0.0, 1.0)
        } else {
            1.0
        };
        let turn = (b.yaw - a.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU)
            - std::f32::consts::PI;
        Some(Pose {
            pos: a.pos.lerp(b.pos, k),
            yaw: a.yaw + turn * k,
            up: a.up.lerp(b.up, k).normalize_or(Vec3::Y),
            vel: a.vel.lerp(b.vel, k),
            steer: a.steer + (b.steer - a.steer) * k,
            throttle: a.throttle + (b.throttle - a.throttle) * k,
        })
    }
}

/// Letters and digits kept, runs of anything else one dash: safe in a file name everywhere.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c);
        } else if c == '\'' {
            // "Tauren's" reads better as "Taurens" than "Tauren-s".
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_string()
}

/// The name a player starts with: the one the page remembers from the multiplayer form, else the
/// computer's user name.
pub fn default_player_name() -> String {
    let name = file_io::remembered_name().unwrap_or_default();
    let name: String = name.trim().chars().take(NAME_MAX).collect();
    if name.is_empty() {
        "driver".into()
    } else {
        name
    }
}

/// The run being recorded, and the last one finished.
#[derive(Resource, Default)]
pub struct Recorder {
    frames: Vec<Frame>,
    course: Option<usize>,
    /// The run just finished, until the next one starts.
    pub last_run: Option<Replay>,
    /// What the last save said: where it went, or why it failed.
    pub saved: Option<Result<String, String>>,
}

/// How a finished run did against the course's gold medal replay.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GoldResult {
    Earned { gold: f32 },
    Missed { gold: f32 },
}

/// The gold medal replays and the ghost that drives them.
#[derive(Resource)]
pub struct GoldGhosts {
    /// Each `goldMedalReplay` path, read once: `None` when it is missing or not a replay.
    loaded: HashMap<String, Option<Replay>>,
    /// The ghost is shown (H).
    pub visible: bool,
    /// Courses whose gold medal has been won.
    pub medals: HashSet<String>,
    /// The last finished run against the gold replay.
    pub result: Option<GoldResult>,
}

impl Default for GoldGhosts {
    fn default() -> Self {
        Self {
            loaded: HashMap::new(),
            visible: true,
            medals: HashSet::new(),
            result: None,
        }
    }
}

impl GoldGhosts {
    /// The course's gold medal replay, read on first ask.
    pub fn load(&mut self, trial: &TimeTrial) -> Option<&Replay> {
        let path = trial.gold_medal_replay.as_ref()?;
        if self.loaded.contains_key(path) {
            return self.loaded.get(path)?.as_ref();
        }
        self.loaded
            .entry(path.clone())
            .or_insert_with(|| {
                let text = file_io::read_asset(path)?;
                match Replay::parse(&text) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        eprintln!("benilla-lambo: gold medal replay {path}: {e}");
                        None
                    }
                }
            })
            .as_ref()
    }

    /// The course's gold medal replay, if it has been read.
    pub fn get(&self, trial: &TimeTrial) -> Option<&Replay> {
        self.loaded.get(trial.gold_medal_replay.as_ref()?)?.as_ref()
    }
}

/// The replay the Replays page opened, or why it could not.
#[derive(Resource, Default)]
pub struct ReplayLibrary {
    pub opened: Option<Replay>,
    pub error: Option<String>,
    /// An open dialog has been asked for and not answered.
    pub picking: bool,
}

/// The replay being watched.
#[derive(Resource)]
pub struct Playback {
    pub replay: Option<Replay>,
    /// Race time shown.
    pub t: f32,
    pub paused: bool,
    pub speed: f32,
    /// Where the watched car is this frame, for `car::drive`.
    pub pose: Option<Pose>,
    /// The gates the replayed car has passed by `sim_t`, frame by frame.
    sim: usize,
    sim_t: f32,
    sim_progress: RaceProgress,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            replay: None,
            t: 0.0,
            paused: false,
            speed: 1.0,
            pose: None,
            sim: 0,
            sim_t: 0.0,
            sim_progress: RaceProgress::default(),
        }
    }
}

/// Everything the menu and HUD need of the replays.
#[derive(SystemParam)]
pub struct ReplayState<'w> {
    pub library: ResMut<'w, ReplayLibrary>,
    pub recorder: ResMut<'w, Recorder>,
    pub gold: ResMut<'w, GoldGhosts>,
    pub playback: ResMut<'w, Playback>,
}

pub struct ReplayPlugin;

impl Plugin for ReplayPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Recorder>()
            .init_resource::<GoldGhosts>()
            .init_resource::<ReplayLibrary>()
            .init_resource::<Playback>()
            .add_systems(
                PreUpdate,
                hold_keys_while_typing.after(bevy_egui::EguiPreUpdateSet::ProcessInput),
            )
            .add_systems(Update, receive_opened_file)
            .add_systems(
                bevy_egui::EguiPrimaryContextPass,
                (replay_hud, ghost_tag, ghost_hint_overlay),
            );
    }
}

/// While a text field has the keyboard (the player's name), the game sees no keys: typing an R
/// would restart the run.
fn hold_keys_while_typing(
    wants: Option<Res<bevy_egui::input::EguiWantsInput>>,
    mut keys: ResMut<ButtonInput<KeyCode>>,
) {
    if wants.is_some_and(|w| w.wants_any_keyboard_input()) {
        keys.reset_all();
    }
}

/// Records the run: a pose every 1/20 s of race time while racing, and at the finish the whole run
/// as a replay, judged against the course's gold medal replay.
pub fn record_runs(
    manager: Res<TimeTrialManager>,
    settings: Res<Settings>,
    car: Query<&Car>,
    mut rec: ResMut<Recorder>,
    mut gold: ResMut<GoldGhosts>,
) {
    let Ok(car) = car.single() else { return };
    let course = manager.current_trial_idx;
    match manager.state {
        TrialState::Racing { elapsed } => {
            // A restart, or another course.
            if rec.course != course || rec.frames.last().is_some_and(|f| elapsed < f.0) {
                rec.frames.clear();
            }
            rec.course = course;
            if rec.frames.is_empty() {
                rec.last_run = None;
                rec.saved = None;
                gold.result = None;
            }
            if rec
                .frames
                .last()
                .is_none_or(|f| elapsed - f.0 >= 1.0 / SAMPLE_HZ)
            {
                rec.frames.push(Frame::of(car, elapsed));
            }
        }
        TrialState::Finished { final_time, .. } => {
            if rec.frames.is_empty() {
                return;
            }
            let Some(trial) = manager.current_trial() else {
                rec.frames.clear();
                return;
            };
            if rec.frames.last().is_some_and(|f| final_time > f.0) {
                rec.frames.push(Frame::of(car, final_time));
            }
            gold.result = gold.load(trial).map(|g| g.time).map(|gold_time| {
                if final_time < gold_time {
                    GoldResult::Earned { gold: gold_time }
                } else {
                    GoldResult::Missed { gold: gold_time }
                }
            });
            if matches!(gold.result, Some(GoldResult::Earned { .. })) {
                gold.medals.insert(trial.name.clone());
            }
            rec.last_run = Some(Replay {
                format: REPLAY_FORMAT.into(),
                version: REPLAY_VERSION,
                player: settings.player_name.clone(),
                course: trial.name.clone(),
                map_id: trial.map_id,
                time: round(final_time, 3),
                lap_times: manager
                    .progress
                    .lap_times
                    .iter()
                    .map(|t| round(*t, 3))
                    .collect(),
                frames: std::mem::take(&mut rec.frames),
            });
            rec.saved = None;
        }
        // The last run stays on the Replays page until the next one starts.
        _ => rec.frames.clear(),
    }
}

/// The gold medal ghost's car.
#[derive(Component)]
pub struct GoldGhost {
    wheels: Wheels,
    spin: f32,
    player: String,
}

/// Drives the gold medal ghost: at the start line until the race begins, then along its replay at
/// the race clock, gone once it has finished. H shows or hides it.
#[allow(clippy::type_complexity)]
pub fn drive_gold_ghost(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    assets: Option<Res<CarAssets>>,
    manager: Res<TimeTrialManager>,
    mut gold: ResMut<GoldGhosts>,
    mut ghosts: Query<(&mut GoldGhost, &mut Transform, &mut Visibility)>,
    mut parts: Query<&mut Transform, Without<GoldGhost>>,
) {
    if keys.just_pressed(GHOST_KEY) && !manager.menu_open {
        gold.visible = !gold.visible;
    }
    let clock = match manager.state {
        TrialState::WaitingToStart | TrialState::Countdown { .. } => Some(0.0),
        TrialState::Racing { elapsed } => Some(elapsed),
        _ => None,
    };
    let visible = gold.visible;
    let pose = clock.zip(manager.current_trial()).and_then(|(t, trial)| {
        let replay = gold.load(trial)?;
        // Done with its run: off the track.
        if t > replay.duration() + 0.5 {
            return None;
        }
        Some((replay.pose_at(t)?, replay.player.clone()))
    });

    let Some((pose, player)) = pose.filter(|_| visible) else {
        for (_, _, mut vis) in &mut ghosts {
            vis.set_if_neq(Visibility::Hidden);
        }
        return;
    };
    let Ok((mut ghost, mut tf, mut vis)) = ghosts.single_mut() else {
        if let Some(assets) = assets {
            let e = commands
                .spawn((
                    Transform::from_translation(pose.pos)
                        .with_rotation(crate::car::orientation(pose.yaw, pose.up)),
                    Visibility::Visible,
                ))
                .id();
            let wheels = spawn_model(&mut commands, &assets, e, true);
            commands.entity(e).insert(GoldGhost {
                wheels,
                spin: 0.0,
                player,
            });
        }
        return;
    };
    vis.set_if_neq(Visibility::Visible);
    ghost.player = player;
    tf.translation = pose.pos;
    tf.rotation = crate::car::orientation(pose.yaw, pose.up);
    let dt = time.delta_secs().min(1.0 / 20.0);
    let forward = Vec3::new(-pose.yaw.sin(), 0.0, -pose.yaw.cos());
    ghost.spin -= pose.vel.dot(forward) / WHEEL_RADIUS * dt;
    let wheels = ghost.wheels;
    for (wheel, steer) in [
        (wheels.back, 0.0),
        (wheels.front[0], -pose.steer),
        (wheels.front[1], -pose.steer),
    ] {
        if let Ok(mut wtf) = parts.get_mut(wheel) {
            wtf.rotation = wheel_rotation(steer, -ghost.spin);
        }
    }
}

/// "AUTHOR TIME" over the ghost, with who set it.
fn ghost_tag(
    mut contexts: EguiContexts,
    ghosts: Query<(&GoldGhost, &GlobalTransform, &ViewVisibility)>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
) {
    let Ok((ghost, gt, seen)) = ghosts.single() else {
        return;
    };
    let (Ok(ctx), Ok((camera, cam_tf))) = (contexts.ctx_mut(), camera.single()) else {
        return;
    };
    if !seen.get() {
        return;
    }
    let at = gt.translation() + Vec3::Y * 2.2;
    if cam_tf.translation().distance(at) > 400.0
        || (at - cam_tf.translation()).dot(cam_tf.forward().as_vec3()) <= 0.0
    {
        return;
    }
    let Ok(p) = camera.world_to_viewport(cam_tf, at) else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::new("gold_ghost_tag"),
    ));
    // Two lines in a gold frame, unlike a driver's plain tag: this is the course's author time, not
    // a player on the road.
    let title =
        painter.layout_no_wrap("AUTHOR TIME".into(), egui::FontId::proportional(15.0), GOLD);
    let sub = painter.layout_no_wrap(
        format!("gold medal ghost · set by {}", ghost.player),
        egui::FontId::proportional(11.0),
        egui::Color32::from_rgb(230, 215, 170),
    );
    let size = egui::vec2(
        title.size().x.max(sub.size().x),
        title.size().y + sub.size().y,
    );
    let rect = egui::Align2::CENTER_BOTTOM.anchor_size(egui::pos2(p.x, p.y), size);
    painter.rect(
        rect.expand2(egui::vec2(7.0, 3.0)),
        4.0,
        egui::Color32::from_black_alpha(170),
        egui::Stroke::new(1.5_f32, GOLD),
        egui::StrokeKind::Outside,
    );
    let mid = rect.center().x;
    painter.galley(
        egui::pos2(mid - title.size().x / 2.0, rect.min.y),
        title.clone(),
        GOLD,
    );
    painter.galley(
        egui::pos2(mid - sub.size().x / 2.0, rect.min.y + title.size().y),
        sub,
        GOLD,
    );
}

/// Puts `replay` on the local car: its course and map are loaded, its gates stand, and the car
/// follows it from the first frame.
pub fn start_watching(
    replay: Replay,
    manager: &mut TimeTrialManager,
    playback: &mut Playback,
    car: &mut Query<&mut Car>,
    chase_cam: &mut Query<&mut ChaseCam>,
    current_map: &mut CurrentMap,
) {
    let course = manager
        .trials
        .iter()
        .position(|t| t.name == replay.course && t.map_id == replay.map_id)
        .or_else(|| manager.trials.iter().position(|t| t.name == replay.course));
    manager.current_trial_idx = course;
    manager.state = TrialState::Replay;
    manager.progress = RaceProgress::default();
    manager.versus_known.clear();
    manager.on_grid = false;
    manager.loading = course.is_some();
    manager.menu_open = false;
    if current_map.0 != replay.map_id {
        current_map.0 = replay.map_id;
    }
    if let Some(first) = replay.frames.first().map(Frame::pose) {
        if let Ok(mut c) = car.single_mut() {
            c.set_spawn(bevy_to_wow(first.pos), first.yaw);
        }
        if let Ok(mut cam) = chase_cam.single_mut() {
            cam.yaw = first.yaw;
            cam.snapped = false;
        }
    }
    eprintln!(
        "benilla-lambo: watching {} on '{}' ({:.2}s)",
        replay.player, replay.course, replay.time
    );
    *playback = Playback {
        replay: Some(replay),
        ..Playback::default()
    };
}

/// Ends a replay: the car drives on from where the replay left it.
fn stop_watching(
    manager: &mut TimeTrialManager,
    playback: &mut Playback,
    car: &mut Query<&mut Car>,
) {
    if let (Some(pose), Ok(mut c)) = (playback.pose, car.single_mut()) {
        let mut at = bevy_to_wow(pose.pos);
        at[2] += 3.0;
        c.set_spawn(at, pose.yaw);
    }
    manager.current_trial_idx = None;
    manager.state = TrialState::FreeDriving;
    manager.progress = RaceProgress::default();
    *playback = Playback::default();
}

/// Moves a watched replay on: its clock (held while the course loads), the pose `car::drive` puts
/// the car in, and the gates its car has passed. Space pauses, the arrows skip and change the
/// speed, R starts it over.
pub fn advance_playback(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut manager: ResMut<TimeTrialManager>,
    playback: ResMut<Playback>,
) {
    let pb = playback.into_inner();
    if manager.state != TrialState::Replay {
        if pb.replay.is_some() || pb.pose.is_some() {
            *pb = Playback::default();
        }
        return;
    }
    let Some(replay) = &pb.replay else { return };
    let end = replay.duration();

    if !manager.menu_open {
        if keys.just_pressed(KeyCode::Space) {
            if pb.paused && pb.t >= end {
                pb.t = 0.0;
            }
            pb.paused = !pb.paused;
        }
        if keys.just_pressed(KeyCode::ArrowLeft) {
            pb.t -= SKIP;
        }
        if keys.just_pressed(KeyCode::ArrowRight) {
            pb.t += SKIP;
        }
        if keys.just_pressed(KeyCode::KeyR) || keys.just_pressed(KeyCode::Delete) {
            pb.t = 0.0;
            pb.paused = false;
        }
        let step = SPEEDS.iter().position(|s| *s >= pb.speed).unwrap_or(2);
        if keys.just_pressed(KeyCode::ArrowUp) {
            pb.speed = SPEEDS[(step + 1).min(SPEEDS.len() - 1)];
        }
        if keys.just_pressed(KeyCode::ArrowDown) {
            pb.speed = SPEEDS[step.saturating_sub(1)];
        }
    }
    let loading = manager.loading && pb.t <= 0.0;
    if !pb.paused && !loading && !manager.menu_open {
        pb.t += time.delta_secs() * pb.speed;
    }
    if pb.t >= end {
        pb.t = end;
        pb.paused = true;
    }
    pb.t = pb.t.max(0.0);
    pb.pose = replay.pose_at(pb.t);

    // The gates: replayed frame by frame from the start, over again after a skip back.
    let Some(trial) = manager.current_trial().cloned() else {
        return;
    };
    if pb.t < pb.sim_t {
        pb.sim = 0;
        pb.sim_progress = RaceProgress::default();
    }
    while let Some(f) = replay.frames.get(pb.sim).filter(|f| f.0 <= pb.t) {
        advance_race(&trial, &mut pb.sim_progress, Vec3::from(f.1), f.0);
        pb.sim += 1;
    }
    pb.sim_t = pb.t;
    if manager.progress != pb.sim_progress {
        manager.progress = pb.sim_progress.clone();
    }
}

/// Picks up a replay file the open dialog has delivered.
fn receive_opened_file(mut library: ResMut<ReplayLibrary>) {
    if !library.picking {
        return;
    }
    let Some(result) = file_io::take_opened() else {
        return;
    };
    library.picking = false;
    match result.and_then(|text| Replay::parse(&text)) {
        Ok(r) => {
            library.opened = Some(r);
            library.error = None;
        }
        Err(e) => library.error = Some(e),
    }
}

/// Saves the run just finished, under the player's current name.
pub fn save_last_run(recorder: &mut Recorder, player: &str) {
    let Some(run) = recorder.last_run.as_mut() else {
        return;
    };
    run.player = if player.trim().is_empty() {
        "driver".into()
    } else {
        player.trim().to_string()
    };
    let saved = file_io::save(&run.file_name(), &run.to_json());
    // Cancelled: nothing to say.
    if let Ok(None) = saved {
        return;
    }
    recorder.saved = Some(saved.map(|at| at.unwrap_or_default()));
}

/// The finish panel's replay lines: the gold medal, and the save button.
pub fn finish_panel(ui: &mut egui::Ui, state: &mut ReplayState, trial: &TimeTrial, player: &str) {
    ui.add_space(4.0);
    match state.gold.result {
        Some(GoldResult::Earned { gold }) => {
            ui.label(
                egui::RichText::new(format!(
                    "🥇 GOLD MEDAL! You beat the author time ({})",
                    format_time(gold)
                ))
                .color(GOLD)
                .size(20.0)
                .strong(),
            );
        }
        Some(GoldResult::Missed { gold }) => {
            let by = state
                .recorder
                .last_run
                .as_ref()
                .map_or(0.0, |r| r.time - gold);
            ui.label(
                egui::RichText::new(format!(
                    "Gold medal (author time) {} — {} off it",
                    format_time(gold),
                    format_time(by.max(0.0))
                ))
                .color(egui::Color32::from_rgb(220, 190, 120))
                .size(14.0),
            );
        }
        None if trial.gold_medal_replay.is_some() && state.gold.get(trial).is_none() => {
            ui.label(
                egui::RichText::new("No gold medal replay for this course yet")
                    .color(egui::Color32::GRAY)
                    .size(12.0),
            );
        }
        None => {}
    }
    if state.recorder.last_run.is_none() {
        return;
    }
    ui.add_space(4.0);
    let save = egui::Button::new(
        egui::RichText::new("💾  Save replay")
            .color(egui::Color32::BLACK)
            .size(16.0)
            .strong(),
    )
    .fill(egui::Color32::from_rgb(255, 170, 0))
    .corner_radius(6.0)
    .min_size(egui::vec2(180.0, 32.0));
    if ui
        .add(save)
        .on_hover_text("Save this run to a file, to watch later or share")
        .clicked()
    {
        save_last_run(&mut state.recorder, player);
    }
    ui.label(
        egui::RichText::new(format!(
            "as \"{player}\" — change your name in ESC › Settings"
        ))
        .color(egui::Color32::GRAY)
        .size(11.0),
    );
    match &state.recorder.saved {
        Some(Ok(at)) => {
            ui.label(
                egui::RichText::new(if at.is_empty() {
                    "Saved".to_string()
                } else {
                    format!("Saved to {at}")
                })
                .color(egui::Color32::from_rgb(110, 220, 120))
                .size(12.0),
            );
        }
        Some(Err(e)) => {
            ui.label(
                egui::RichText::new(format!("Could not save: {e}"))
                    .color(egui::Color32::from_rgb(255, 90, 80))
                    .size(12.0),
            );
        }
        None => {}
    }
}

/// The race HUD's gold medal lines: the gold time, and before the start the ghost key. The first
/// seconds of the race say it mid-screen instead (`ghost_hint_overlay`).
pub fn race_lines(ui: &mut egui::Ui, gold: &GoldGhosts, trial: &TimeTrial, state: &TrialState) {
    let (start, overlay) = match *state {
        TrialState::WaitingToStart | TrialState::Countdown { .. } => (true, false),
        TrialState::Racing { elapsed } => (false, elapsed < GHOST_HINT_SECS),
        _ => return,
    };
    let Some(replay) = gold.get(trial) else {
        if start {
            ui.label(
                egui::RichText::new("This course has no gold medal ghost yet")
                    .color(egui::Color32::GRAY)
                    .size(12.0),
            );
        }
        return;
    };
    ui.label(
        egui::RichText::new(format!(
            "🥇 Gold medal: beat {} (author time by {})",
            format_time(replay.time),
            replay.player,
        ))
        .color(GOLD)
        .size(13.0),
    );
    if start || (!gold.visible && !overlay) {
        ui.label(
            egui::RichText::new(ghost_hint(gold.visible))
                .color(GOLD)
                .size(if start { 15.0 } else { 12.0 })
                .strong(),
        );
    }
}

/// The ghost key, large in the middle of the screen for the first seconds of a race on a course
/// with a gold medal ghost, fading out over the last half second.
fn ghost_hint_overlay(
    mut contexts: EguiContexts,
    manager: Res<TimeTrialManager>,
    gold: Res<GoldGhosts>,
) {
    let TrialState::Racing { elapsed } = manager.state else {
        return;
    };
    if elapsed >= GHOST_HINT_SECS || manager.menu_open {
        return;
    }
    if manager.current_trial().and_then(|t| gold.get(t)).is_none() {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let fade = ((GHOST_HINT_SECS - elapsed) / 0.5).clamp(0.0, 1.0);
    let alpha = |c: egui::Color32| c.gamma_multiply(fade);
    egui::Area::new(egui::Id::new("ghost_hint_overlay"))
        .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, -60.0))
        .interactable(false)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            egui::Frame::new()
                .fill(alpha(egui::Color32::from_black_alpha(190)))
                .stroke(egui::Stroke::new(2.0_f32, alpha(GOLD)))
                .corner_radius(10.0)
                .inner_margin(egui::Margin::symmetric(28, 14))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new(ghost_hint(gold.visible))
                                .color(alpha(GOLD))
                                .size(26.0)
                                .strong(),
                        );
                    });
                });
        });
}

/// The ghost key's hint, the same shown or hidden.
pub fn ghost_hint(_visible: bool) -> &'static str {
    "H to toggle"
}

/// What the Replays page asks for.
pub enum ReplayAction {
    Watch(Replay),
}

/// The ESC menu's Replays page: open a replay file, watch the run just finished, or watch a
/// course's gold medal replay.
pub fn replays_page(
    ui: &mut egui::Ui,
    state: &mut ReplayState,
    trials: &[TimeTrial],
) -> Option<ReplayAction> {
    let mut action = None;
    let heading = |ui: &mut egui::Ui, text: &str| {
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new(text)
                .color(egui::Color32::from_rgb(255, 140, 0))
                .size(13.0)
                .strong(),
        );
    };
    let card = |ui: &mut egui::Ui, replay: &Replay, label: &str| -> bool {
        let mut go = false;
        egui::Frame::new()
            .fill(egui::Color32::from_white_alpha(6))
            .stroke(egui::Stroke::new(
                1.0_f32,
                egui::Color32::from_white_alpha(18),
            ))
            .corner_radius(6.0)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            egui::RichText::new(&replay.course)
                                .size(15.0)
                                .strong()
                                .color(egui::Color32::WHITE),
                        );
                        ui.label(
                            egui::RichText::new(format!(
                                "{} • {}",
                                replay.player,
                                format_time(replay.time)
                            ))
                            .size(12.0)
                            .color(egui::Color32::GRAY),
                        );
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        go = ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(label)
                                        .color(egui::Color32::BLACK)
                                        .strong(),
                                )
                                .fill(egui::Color32::from_rgb(110, 220, 120))
                                .corner_radius(6.0)
                                .min_size(egui::vec2(90.0, 28.0)),
                            )
                            .clicked();
                    });
                });
            });
        go
    };

    heading(ui, "OPEN A REPLAY FILE");
    ui.horizontal(|ui| {
        let open = ui.add_enabled(
            !state.library.picking,
            egui::Button::new(egui::RichText::new("📂  Open replay…").size(14.0))
                .corner_radius(6.0)
                .min_size(egui::vec2(150.0, 28.0)),
        );
        if open.clicked() {
            state.library.picking = true;
            state.library.error = None;
            file_io::request_open();
        }
        if state.library.picking {
            ui.label(egui::RichText::new("choosing…").color(egui::Color32::GRAY));
        }
    });
    if let Some(e) = &state.library.error {
        ui.label(
            egui::RichText::new(format!("Could not open it: {e}"))
                .color(egui::Color32::from_rgb(255, 90, 80))
                .size(12.0),
        );
    }
    if let Some(r) = &state.library.opened {
        if !trials.iter().any(|t| t.name == r.course) {
            ui.label(
                egui::RichText::new(
                    "This course is not in your list: the car replays without its gates.",
                )
                .color(egui::Color32::GRAY)
                .size(11.0),
            );
        }
        if card(ui, r, "▶  Watch") {
            action = Some(ReplayAction::Watch(r.clone()));
        }
    }

    if let Some(run) = &state.recorder.last_run {
        heading(ui, "YOUR LAST RUN");
        if card(ui, run, "▶  Watch") {
            action = Some(ReplayAction::Watch(run.clone()));
        }
    }

    heading(ui, "GOLD MEDAL REPLAYS");
    let mut any = false;
    for trial in trials {
        let Some(r) = state.gold.load(trial).cloned() else {
            continue;
        };
        any = true;
        let won = state.gold.medals.contains(&trial.name);
        // The card says whose run it is: the course's author time, not a player's.
        let shown = Replay {
            player: format!("Author time by {}", r.player),
            frames: Vec::new(),
            ..r.clone()
        };
        if card(ui, &shown, if won { "🥇 Watch" } else { "▶  Watch" }) {
            action = Some(ReplayAction::Watch(r));
        }
        ui.add_space(4.0);
    }
    if !any {
        ui.label(
            egui::RichText::new(
                "None yet. Save a run and put it at a course's goldMedalReplay path in time_trials.json.",
            )
            .color(egui::Color32::GRAY)
            .size(12.0),
        );
    }
    ui.add_space(8.0);
    ui.label(
        egui::RichText::new(
            "While watching: Space pause · ← → skip 5 s · ↑ ↓ speed · R restart · ESC menu",
        )
        .color(egui::Color32::GRAY)
        .size(11.0),
    );
    action
}

/// The replay viewer's panel: who drove, the clock, a scrub bar and the controls.
fn replay_hud(
    mut contexts: EguiContexts,
    mut manager: ResMut<TimeTrialManager>,
    mut playback: ResMut<Playback>,
    mut car: Query<&mut Car>,
) {
    if manager.state != TrialState::Replay || manager.menu_open {
        return;
    }
    let Ok(ctx) = contexts.ctx_mut() else { return };
    let Some(replay) = playback.replay.as_ref() else {
        return;
    };
    let (course, player, total, end) = (
        replay.course.clone(),
        replay.player.clone(),
        replay.time,
        replay.duration(),
    );
    let label = manager.race_label();
    let loading = manager.loading && playback.t <= 0.0;
    let mut exit = false;

    egui::Area::new(egui::Id::new("replay_title"))
        .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 16.0))
        .interactable(false)
        .show(ctx, |ui| {
            egui::Frame::window(&ctx.style())
                .fill(egui::Color32::from_black_alpha(200))
                .stroke(egui::Stroke::new(
                    1.5_f32,
                    egui::Color32::from_rgb(120, 200, 255),
                ))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(24, 10))
                .show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.label(
                            egui::RichText::new("▶ REPLAY")
                                .color(egui::Color32::from_rgb(120, 200, 255))
                                .size(13.0)
                                .strong(),
                        );
                        ui.heading(
                            egui::RichText::new(&course)
                                .color(egui::Color32::WHITE)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!("{player} • {}", format_time(total)))
                                .color(egui::Color32::LIGHT_GRAY)
                                .size(14.0),
                        );
                        ui.label(
                            egui::RichText::new(format_time(playback.t))
                                .color(egui::Color32::from_rgb(255, 230, 80))
                                .size(28.0)
                                .monospace()
                                .strong(),
                        );
                        if !label.is_empty() {
                            ui.label(
                                egui::RichText::new(&label)
                                    .color(egui::Color32::from_rgb(120, 220, 255))
                                    .size(14.0),
                            );
                        }
                        if loading {
                            ui.label(
                                egui::RichText::new("LOADING MAP…")
                                    .color(egui::Color32::from_rgb(255, 230, 80))
                                    .size(14.0),
                            );
                        }
                    });
                });
        });

    egui::Area::new(egui::Id::new("replay_controls"))
        .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -24.0))
        .show(ctx, |ui| {
            egui::Frame::window(&ctx.style())
                .fill(egui::Color32::from_black_alpha(210))
                .stroke(egui::Stroke::new(
                    1.0_f32,
                    egui::Color32::from_white_alpha(40),
                ))
                .corner_radius(8.0)
                .inner_margin(egui::Margin::symmetric(16, 10))
                .show(ui, |ui| {
                    ui.set_width(520.0_f32.min(ctx.content_rect().width() - 300.0).max(260.0));
                    let mut t = playback.t;
                    ui.spacing_mut().slider_width = ui.available_width();
                    if ui
                        .add(egui::Slider::new(&mut t, 0.0..=end.max(0.01)).show_value(false))
                        .changed()
                    {
                        playback.t = t;
                    }
                    ui.horizontal(|ui| {
                        let play = if playback.paused {
                            "▶ Play"
                        } else {
                            "⏸ Pause"
                        };
                        if ui.button(play).clicked() {
                            if playback.paused && playback.t >= end {
                                playback.t = 0.0;
                            }
                            playback.paused = !playback.paused;
                        }
                        if ui.button("⏮ Restart").clicked() {
                            playback.t = 0.0;
                            playback.paused = false;
                        }
                        ui.separator();
                        for s in SPEEDS {
                            let text = if s < 1.0 {
                                format!("{s}×")
                            } else {
                                format!("{s:.0}×")
                            };
                            if ui
                                .selectable_label((playback.speed - s).abs() < 1e-3, text)
                                .clicked()
                            {
                                playback.speed = s;
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            exit = ui.button("✖ Exit replay").clicked();
                        });
                    });
                });
        });

    if exit {
        stop_watching(&mut manager, &mut playback, &mut car);
    }
}

/// Saving and opening replay files: a native dialog on a desktop, a download and the browser's
/// file picker on the page.
#[cfg(not(target_arch = "wasm32"))]
mod file_io {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::REPLAY_EXT;

    /// The file the last open dialog chose, read.
    static OPENED: Mutex<Option<Result<String, String>>> = Mutex::new(None);

    pub fn remembered_name() -> Option<String> {
        std::env::var("USERNAME")
            .or_else(|_| std::env::var("USER"))
            .ok()
    }

    /// Asks where to save, and saves: the path written, or `None` when the player cancelled.
    pub fn save(name: &str, text: &str) -> Result<Option<String>, String> {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save replay")
            .set_file_name(name)
            .add_filter("benilla lambo replay", &[REPLAY_EXT])
            .save_file()
        else {
            return Ok(None);
        };
        std::fs::write(&path, text).map_err(|e| e.to_string())?;
        Ok(Some(path.display().to_string()))
    }

    pub fn request_open() {
        let picked = rfd::FileDialog::new()
            .set_title("Open replay")
            .add_filter("benilla lambo replay", &[REPLAY_EXT, "json"])
            .pick_file();
        let result = match picked {
            Some(path) => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
            None => Err("no file chosen".into()),
        };
        *OPENED.lock().unwrap_or_else(|p| p.into_inner()) = Some(result);
    }

    pub fn take_opened() -> Option<Result<String, String>> {
        OPENED.lock().unwrap_or_else(|p| p.into_inner()).take()
    }

    /// A `goldMedalReplay` path: absolute, or under the crate's `assets`, beside the courses
    /// file, or from the working directory.
    pub fn read_asset(path: &str) -> Option<String> {
        let rel = Path::new(path);
        let mut candidates: Vec<PathBuf> = vec![];
        if rel.is_absolute() {
            candidates.push(rel.to_path_buf());
        } else {
            if let Ok(file) = std::env::var("TIME_TRIALS_FILE") {
                if let Some(dir) = Path::new(&file).parent() {
                    candidates.push(dir.join(rel));
                }
            }
            candidates.push(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("assets")
                    .join(rel),
            );
            candidates.push(Path::new("assets").join(rel));
            candidates.push(rel.to_path_buf());
        }
        candidates
            .iter()
            .find_map(|p| std::fs::read_to_string(p).ok())
    }
}

#[cfg(target_arch = "wasm32")]
mod file_io {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen(inline_js = r#"
let picked = null;
export function replay_download(name, text) {
  const url = URL.createObjectURL(new Blob([text], { type: "application/json" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}
export function replay_pick() {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = ".lambo-replay,.json";
  input.onchange = async () => {
    const f = input.files && input.files[0];
    if (!f) { picked = { err: "no file chosen" }; return; }
    try { picked = { text: await f.text() }; } catch (e) { picked = { err: String(e) }; }
  };
  input.oncancel = () => { picked = { err: "no file chosen" }; };
  input.click();
}
export function replay_take_text() {
  if (!picked || picked.text === undefined) return undefined;
  const t = picked.text; picked = null; return t;
}
export function replay_take_error() {
  if (!picked || picked.err === undefined) return undefined;
  const e = picked.err; picked = null; return e;
}
export function replay_name() {
  try { return localStorage.getItem("lambo-name") || undefined; } catch { return undefined; }
}
"#)]
    extern "C" {
        fn replay_download(name: &str, text: &str);
        fn replay_pick();
        fn replay_take_text() -> Option<String>;
        fn replay_take_error() -> Option<String>;
        fn replay_name() -> Option<String>;
    }

    pub fn remembered_name() -> Option<String> {
        replay_name()
    }

    /// Hands the file to the browser as a download, which lands in the player's downloads.
    pub fn save(name: &str, text: &str) -> Result<Option<String>, String> {
        replay_download(name, text);
        Ok(Some(format!("your downloads ({name})")))
    }

    pub fn request_open() {
        replay_pick();
    }

    pub fn take_opened() -> Option<Result<String, String>> {
        replay_take_text()
            .map(Ok)
            .or_else(|| replay_take_error().map(Err))
    }

    /// A `goldMedalReplay` file the page fetched ahead of the start (`web/index.html`).
    pub fn read_asset(path: &str) -> Option<String> {
        String::from_utf8(crate::web::asset(path)?).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run() -> Replay {
        Replay {
            format: REPLAY_FORMAT.into(),
            version: REPLAY_VERSION,
            player: "Slayzer".into(),
            course: "Tauren's Track".into(),
            map_id: 1,
            time: 63.21,
            lap_times: vec![63.21],
            frames: vec![
                Frame(
                    0.0,
                    [0.0, 0.0, 0.0],
                    0.0,
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, -10.0],
                    0.0,
                    1.0,
                ),
                Frame(
                    1.0,
                    [0.0, 0.0, -10.0],
                    1.0,
                    [0.0, 1.0, 0.0],
                    [0.0, 0.0, -10.0],
                    0.2,
                    1.0,
                ),
                // A respawn: 200 yards on.
                Frame(
                    1.05,
                    [200.0, 0.0, -10.0],
                    1.0,
                    [0.0, 1.0, 0.0],
                    [0.0; 3],
                    0.0,
                    0.0,
                ),
                Frame(
                    63.21,
                    [210.0, 0.0, -10.0],
                    1.0,
                    [0.0, 1.0, 0.0],
                    [0.0; 3],
                    0.0,
                    0.0,
                ),
            ],
        }
    }

    #[test]
    fn a_replay_survives_its_file_with_the_player_name() {
        let r = run();
        let back = Replay::parse(&r.to_json()).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.player, "Slayzer");
    }

    /// Every gold medal replay that ships reads, and was driven on the course that names it.
    #[test]
    fn the_shipped_gold_replays_are_their_courses() {
        let trials = crate::trial::load_time_trials();
        let mut gold = GoldGhosts::default();
        let mut found = 0;
        for t in &trials {
            let path = t.gold_medal_replay.clone().unwrap_or_default();
            if file_io::read_asset(&path).is_none() {
                continue;
            }
            let r = gold
                .load(t)
                .unwrap_or_else(|| panic!("{path} does not read"));
            assert_eq!((r.course.as_str(), r.map_id), (t.name.as_str(), t.map_id));
            found += 1;
        }
        assert!(found >= 8, "only {found} gold replays found");
    }

    #[test]
    fn a_file_that_is_not_a_replay_is_refused() {
        assert!(Replay::parse("{}").is_err());
        let mut r = run();
        r.format = "something".into();
        assert!(Replay::parse(&r.to_json()).is_err());
        let mut r = run();
        r.frames.clear();
        assert!(Replay::parse(&r.to_json()).is_err());
        let mut r = run();
        r.frames.swap(0, 1);
        assert!(Replay::parse(&r.to_json()).is_err());
    }

    #[test]
    fn playback_eases_between_frames_and_jumps_a_respawn() {
        let r = run();
        let mid = r.pose_at(0.5).unwrap();
        assert!((mid.pos.z + 5.0).abs() < 1e-4);
        assert!((mid.yaw - 0.5).abs() < 1e-4);
        // Between the frames either side of the respawn it holds the one before.
        let jump = r.pose_at(1.02).unwrap();
        assert_eq!(jump.pos, Vec3::new(0.0, 0.0, -10.0));
        // Before the start and past the end it holds the ends.
        assert_eq!(r.pose_at(-1.0).unwrap().pos, Vec3::ZERO);
        assert_eq!(r.pose_at(99.0).unwrap().pos, Vec3::new(210.0, 0.0, -10.0));
        assert!((r.duration() - 63.21).abs() < 1e-4);
    }

    #[test]
    fn yaw_eases_the_short_way_round() {
        let mut r = run();
        r.frames[0].2 = 3.0;
        r.frames[1].2 = -3.0;
        let yaw = r.pose_at(0.5).unwrap().yaw;
        // 3.0 to -3.0 passes through pi, not zero.
        assert!(yaw.abs() > 3.0);
    }

    #[test]
    fn the_file_name_is_safe_and_names_the_run() {
        assert_eq!(
            run().file_name(),
            "Taurens-Track_01-03.21_Slayzer.lambo-replay"
        );
        let mut r = run();
        r.player = "a/b\\c: d".into();
        assert_eq!(r.file_name(), "Taurens-Track_01-03.21_a-b-c-d.lambo-replay");
    }
}
