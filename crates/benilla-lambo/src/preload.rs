//! Course preloading: when a time trial or race is picked, the terrain tiles along its route and
//! the models they place are loaded ahead of the car, so the drive never waits on a read.
//!
//! The streamer loads every tile and model through the `AssetServer` by path, so holding a handle
//! to the same path here is all a preload needs: when the car arrives, the streamer's own request
//! finds the asset already decoded. The preload rolls with the car: it holds the next
//! [`MAX_TILES`] tiles of the route from where the car is, and lets go of what falls behind, which
//! Bevy then frees unless the streamer still uses it.

use std::collections::{HashMap, HashSet, VecDeque};

use benilla_assets::coords::bevy_to_wow;
use benilla_assets::{m2_url, wmo_url, AdtTile, M2Model, MapCatalogRes, WdtIndex, WmoModel};
use benilla_formats::CHUNK_SIZE;
use benilla_world::terrain_stream::StreamWindow;
use benilla_world::view::ViewDistance;
use bevy::asset::{LoadState, UntypedHandle};
use bevy::prelude::*;

use crate::car::Car;
use crate::trial::{TimeTrial, TimeTrialManager, TrialState};

/// Yards a driver may stray from the straight line between gates and still find the world loaded.
pub const ROUTE_MARGIN_YD: f32 = 200.0;
/// Tiles held ahead of the car, a few MB each decoded. At the 777-yard view distance the longest
/// shipped course wants 80 along its whole length, so the hold rolls rather than covering it all.
const MAX_TILES: usize = 48;
/// Reads in flight at once, so the preload never crowds out the tiles the car needs now.
#[cfg(not(target_arch = "wasm32"))]
const TILES_IN_FLIGHT: usize = 4;
#[cfg(target_arch = "wasm32")]
const TILES_IN_FLIGHT: usize = 2;
#[cfg(not(target_arch = "wasm32"))]
const MODELS_IN_FLIGHT: usize = 16;
#[cfg(target_arch = "wasm32")]
const MODELS_IN_FLIGHT: usize = 6;

/// The route a car drives: the start, every checkpoint in order, then the finish.
fn route(trial: &TimeTrial) -> Vec<[f32; 3]> {
    let mut points = vec![trial.start];
    points.extend(&trial.checkpoints);
    points.push(trial.end);
    points
}

/// One point of the route, a chunk from the last, and the tiles the streamer wants there.
#[derive(Clone, Debug)]
pub struct Sample {
    /// The leg it lies on: leg `k` runs to checkpoint `k`, the last leg to the finish.
    leg: usize,
    /// WoW `(x, y)`.
    at: Vec2,
    tiles: Vec<(i32, i32)>,
}

/// The route as points a chunk apart, each with the tiles a car there needs: the streamer's window
/// with the view distance widened by `margin` yards, for a driver off the straight line.
pub fn course_samples(trial: &TimeTrial, farclip: f32, margin: f32) -> Vec<Sample> {
    let reach = farclip + margin.max(0.0);
    let sample = |leg: usize, at: Vec2| Sample {
        leg,
        at,
        tiles: StreamWindow::at(reach, at.x, at.y).wanted_tiles(),
    };
    let points = route(trial);
    let mut out = vec![sample(0, Vec2::new(points[0][0], points[0][1]))];
    for (leg, pair) in points.windows(2).enumerate() {
        let (a, b) = (
            Vec2::new(pair[0][0], pair[0][1]),
            Vec2::new(pair[1][0], pair[1][1]),
        );
        let steps = ((b - a).length() / CHUNK_SIZE).ceil().max(1.0) as usize;
        for i in 1..=steps {
            out.push(sample(leg, a.lerp(b, i as f32 / steps as f32)));
        }
    }
    out
}

/// The first `cap` distinct tiles a car meets driving on from sample `from`; a circuit wraps
/// round to its start for the next lap.
pub fn tiles_ahead(samples: &[Sample], from: usize, circuit: bool, cap: usize) -> Vec<(i32, i32)> {
    let from = from.min(samples.len());
    let wrap = if circuit { &samples[..from] } else { &[][..] };
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for t in samples[from..].iter().chain(wrap).flat_map(|s| &s.tiles) {
        if out.len() >= cap {
            break;
        }
        if seen.insert(*t) {
            out.push(*t);
        }
    }
    out
}

/// Every tile the course needs, in the order the car reaches them.
#[cfg(test)]
fn course_tiles(trial: &TimeTrial, farclip: f32, margin: f32) -> Vec<(i32, i32)> {
    tiles_ahead(
        &course_samples(trial, farclip, margin),
        0,
        false,
        usize::MAX,
    )
}

/// The sample the car is at: the nearest on the leg it is driving. Restricting to the leg keeps
/// a route that crosses itself from jumping ahead or back.
fn car_sample(samples: &[Sample], leg: usize, car: Vec2) -> usize {
    let last_leg = samples.last().map_or(0, |s| s.leg);
    let leg = leg.min(last_leg);
    samples
        .iter()
        .enumerate()
        .filter(|(_, s)| s.leg == leg)
        .min_by(|a, b| {
            a.1.at
                .distance_squared(car)
                .total_cmp(&b.1.at.distance_squared(car))
        })
        .map_or(0, |(i, _)| i)
}

/// One held tile: its handle, and the models it places once it has loaded.
struct TileEntry {
    handle: Handle<AdtTile>,
    scanned: bool,
    /// Model URLs not yet requested.
    model_queue: VecDeque<String>,
    models: Vec<UntypedHandle>,
}

/// The preloaded stretch of the current course: the handles that keep it resident.
#[derive(Resource, Default)]
pub struct CoursePreload {
    /// The course and view distance the samples are for.
    key: Option<(usize, u32)>,
    map_dir: Option<String>,
    wdt: Option<Handle<WdtIndex>>,
    /// The route's samples, their tiles filtered by the WDT once it has loaded.
    samples: Vec<Sample>,
    /// The WDT has answered (or failed) and the samples are filtered.
    planned: bool,
    /// The sample the hold was last rolled to.
    cursor: Option<usize>,
    /// The tiles held now, in the order the car reaches them.
    ahead: Vec<(i32, i32)>,
    tiles: HashMap<(i32, i32), TileEntry>,
    /// `(ready, total)` as of the last frame, for the HUD.
    progress: (usize, usize),
}

impl CoursePreload {
    /// Loaded units of the known total while the stretch ahead is still loading; `None` with no
    /// course or once it is all in.
    pub fn pending(&self) -> Option<(usize, usize)> {
        let (ready, total) = self.progress;
        (self.key.is_some() && (!self.planned || ready < total)).then_some(self.progress)
    }
}

/// A load that is over, either way.
fn settled(server: &AssetServer, id: impl Into<bevy::asset::UntypedAssetId>) -> bool {
    matches!(
        server.load_state(id),
        LoadState::Loaded | LoadState::Failed(_)
    )
}

/// Keeps the stretch of the course ahead of the car loaded: samples the route when the course or
/// the view distance changes, rolls the hold forward as the car moves, and requests tiles a few at
/// a time in the order the car reaches them, then the models each one places.
#[allow(clippy::too_many_arguments)]
pub fn preload_course(
    manager: Res<TimeTrialManager>,
    view: Res<ViewDistance>,
    catalog: Option<Res<MapCatalogRes>>,
    server: Res<AssetServer>,
    adts: Res<Assets<AdtTile>>,
    wdts: Res<Assets<WdtIndex>>,
    car: Query<&Car>,
    pre: ResMut<CoursePreload>,
) {
    let pre = pre.into_inner();
    let Some((idx, trial)) = manager
        .current_trial_idx
        .and_then(|i| Some((i, manager.trials.get(i)?)))
    else {
        if pre.key.is_some() {
            *pre = CoursePreload::default();
        }
        return;
    };
    let Some(catalog) = catalog else { return };
    let Some(dir) = catalog.0.directory(trial.map_id).map(str::to_string) else {
        return;
    };

    let key = (idx, view.farclip.round() as u32);
    if pre.key != Some(key) {
        // Another course or map: nothing held is of use. A view-distance change keeps the tiles,
        // and the roll below drops those no longer ahead.
        if pre.key.map(|k| k.0) != Some(idx) || pre.map_dir.as_deref() != Some(dir.as_str()) {
            *pre = CoursePreload::default();
        }
        pre.key = Some(key);
        pre.wdt = Some(server.load(format!("mpq://World/Maps/{dir}/{dir}.wdt")));
        pre.map_dir = Some(dir.clone());
        pre.samples = course_samples(trial, view.farclip, ROUTE_MARGIN_YD);
        pre.planned = false;
        pre.cursor = None;
    }

    // The WDT says which tiles exist; a failed WDT lets every tile be tried.
    if !pre.planned {
        let wdt_handle = pre.wdt.clone();
        let wdt = wdt_handle.as_ref().and_then(|h| wdts.get(h));
        let failed = wdt_handle
            .as_ref()
            .is_some_and(|h| matches!(server.load_state(h), LoadState::Failed(_)));
        if !(wdt.is_some() || failed) {
            return;
        }
        if let Some(w) = wdt {
            for s in &mut pre.samples {
                s.tiles.retain(|&(x, y)| w.has_tile(x as u32, y as u32));
            }
        }
        pre.planned = true;
    }

    // Roll the hold to where the car is; before the start and after the finish, the start.
    let at = match (manager.state.clone(), car.single()) {
        (TrialState::Racing { .. }, Ok(c)) => {
            let wow = bevy_to_wow(c.pos);
            car_sample(
                &pre.samples,
                manager.progress.next_checkpoint,
                Vec2::new(wow[0], wow[1]),
            )
        }
        _ => 0,
    };
    if pre.cursor != Some(at) {
        pre.cursor = Some(at);
        pre.ahead = tiles_ahead(&pre.samples, at, trial.circuit, MAX_TILES);
        let ahead = &pre.ahead;
        pre.tiles.retain(|t, _| ahead.contains(t));
    }

    // Tiles, nearest ahead first, a few reads at a time.
    let mut in_flight = pre
        .tiles
        .values()
        .filter(|t| !settled(&server, &t.handle))
        .count();
    for &(x, y) in &pre.ahead {
        if in_flight >= TILES_IN_FLIGHT {
            break;
        }
        if pre.tiles.contains_key(&(x, y)) {
            continue;
        }
        let handle = server.load(format!("mpq://World/Maps/{dir}/{dir}_{x}_{y}.adt"));
        pre.tiles.insert(
            (x, y),
            TileEntry {
                handle,
                scanned: false,
                model_queue: VecDeque::new(),
                models: Vec::new(),
            },
        );
        in_flight += 1;
    }

    // Each loaded tile queues the doodads and buildings it places.
    for entry in pre.tiles.values_mut().filter(|t| !t.scanned) {
        let Some(adt) = adts.get(&entry.handle) else {
            if matches!(server.load_state(&entry.handle), LoadState::Failed(_)) {
                entry.scanned = true;
            }
            continue;
        };
        entry.scanned = true;
        let mut urls: Vec<String> = adt
            .doodads
            .iter()
            .map(|d| m2_url(&d.model))
            .chain(adt.wmos.iter().map(|w| wmo_url(&w.model)))
            .collect();
        urls.sort_unstable();
        urls.dedup();
        entry.model_queue = urls.into();
    }

    // Models, in the same order, a bounded number in flight; a model two tiles share is one load.
    let mut models_in_flight = pre
        .tiles
        .values()
        .flat_map(|t| &t.models)
        .filter(|h| !settled(&server, *h))
        .count();
    for t in &pre.ahead {
        let Some(entry) = pre.tiles.get_mut(t) else {
            continue;
        };
        while models_in_flight < MODELS_IN_FLIGHT {
            let Some(url) = entry.model_queue.pop_front() else {
                break;
            };
            let handle: UntypedHandle = if url.ends_with(".wmo") {
                server.load::<WmoModel>(url).untyped()
            } else {
                server.load::<M2Model>(url).untyped()
            };
            if !settled(&server, &handle) {
                models_in_flight += 1;
            }
            entry.models.push(handle);
        }
    }

    let (mut ready, mut total) = (0, pre.ahead.len());
    for t in pre.tiles.values() {
        ready += usize::from(t.scanned);
        total += t.model_queue.len() + t.models.len();
        ready += t.models.iter().filter(|h| settled(&server, *h)).count();
    }
    pre.progress = (ready, total);
}

#[cfg(test)]
mod tests {
    use super::*;
    use benilla_formats::TILE_SIZE;

    fn sprint(start: [f32; 3], end: [f32; 3]) -> TimeTrial {
        TimeTrial::sprint("t", 0, start, end, None)
    }

    #[test]
    fn the_route_is_covered_from_start_to_finish_in_order() {
        let t = sprint([-9429.1, -2231.4, 68.0], [-10628.9, 1036.7, 34.0]);
        let tiles = course_tiles(&t, 350.0, ROUTE_MARGIN_YD);
        let at = |x: f32, y: f32| StreamWindow::at(350.0, x, y).focus_tile();
        let (start, end) = (at(t.start[0], t.start[1]), at(t.end[0], t.end[1]));
        // The start's window comes first, the finish after it, and nothing repeats.
        let first =
            StreamWindow::at(350.0 + ROUTE_MARGIN_YD, t.start[0], t.start[1]).wanted_tiles();
        assert_eq!(tiles[..first.len()], first[..]);
        assert!(tiles.contains(&start) && tiles.contains(&end));
        let pos = |t: (i32, i32)| tiles.iter().position(|&x| x == t).unwrap();
        assert!(pos(start) < pos(end));
        let unique: HashSet<_> = tiles.iter().collect();
        assert_eq!(unique.len(), tiles.len());
    }

    #[test]
    fn checkpoints_bend_the_route() {
        // A detour gate a few tiles off the straight line pulls its tiles in.
        let mut t = sprint([0.0, 0.0, 0.0], [0.0, 4.0 * TILE_SIZE, 0.0]);
        let detour = [3.0 * TILE_SIZE, 2.0 * TILE_SIZE, 0.0];
        let straight = course_tiles(&t, 177.0, 0.0);
        t.checkpoints.push(detour);
        let bent = course_tiles(&t, 177.0, 0.0);
        let gate = StreamWindow::at(177.0, detour[0], detour[1]).focus_tile();
        assert!(!straight.contains(&gate));
        assert!(bent.contains(&gate));
    }

    #[test]
    fn the_margin_widens_the_corridor() {
        let t = sprint([0.0, 0.0, 0.0], [0.0, 3.0 * TILE_SIZE, 0.0]);
        let tight = course_tiles(&t, 177.0, 0.0);
        let wide = course_tiles(&t, 177.0, ROUTE_MARGIN_YD + TILE_SIZE);
        assert!(wide.len() > tight.len());
        assert!(tight.iter().all(|t| wide.contains(t)));
    }

    /// At the longest view distance a long course wants more than the hold, so it rolls:
    /// from the start it holds the opening stretch, and nearer the finish the finish's tiles.
    #[test]
    fn the_hold_rolls_with_the_car_at_full_view_distance() {
        // A straight run of twelve tiles: longer than the hold at 777, finish far from the start.
        let long = &sprint([0.0, 0.0, 0.0], [0.0, 12.0 * TILE_SIZE, 0.0]);
        assert!(course_tiles(long, 777.0, ROUTE_MARGIN_YD).len() > MAX_TILES);
        let samples = course_samples(long, 777.0, ROUTE_MARGIN_YD);
        let finish = StreamWindow::at(777.0, long.end[0], long.end[1]).focus_tile();

        let from_start = tiles_ahead(&samples, 0, long.circuit, MAX_TILES);
        assert_eq!(from_start.len(), MAX_TILES);
        assert!(!from_start.contains(&finish));

        let near_end = tiles_ahead(&samples, samples.len() * 3 / 4, long.circuit, MAX_TILES);
        assert!(near_end.contains(&finish));
        assert!(near_end.len() <= MAX_TILES);
    }

    #[test]
    fn a_circuit_holds_the_next_lap_past_the_line() {
        let mut t = sprint([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        t.circuit = true;
        t.checkpoints = vec![
            [0.0, 4.0 * TILE_SIZE, 0.0],
            [4.0 * TILE_SIZE, 4.0 * TILE_SIZE, 0.0],
            [4.0 * TILE_SIZE, 0.0, 0.0],
        ];
        let samples = course_samples(&t, 177.0, 0.0);
        let last = samples.len() - 1;
        // A circuit's finish is its start, so from the second sample on the second leg the
        // first leg's far end comes round again.
        let second_leg = samples.iter().position(|s| s.leg == 1).unwrap() + 1;
        let first_leg_mid = &samples[second_leg / 2].tiles;
        assert!(tiles_ahead(&samples, second_leg, true, usize::MAX)
            .iter()
            .any(|x| first_leg_mid.contains(x)));
        // A sprint never wraps.
        assert_eq!(tiles_ahead(&samples, last + 1, false, usize::MAX), vec![]);
    }

    #[test]
    fn the_car_is_placed_on_its_own_leg() {
        let mut t = sprint([0.0, 0.0, 0.0], [0.0, 0.0, 0.0]);
        // Out and back along one line: legs 0 and 1 overlap.
        t.checkpoints = vec![[0.0, 3.0 * TILE_SIZE, 0.0]];
        let samples = course_samples(&t, 177.0, 0.0);
        let mid = Vec2::new(0.0, 1.5 * TILE_SIZE);
        assert_eq!(samples[car_sample(&samples, 0, mid)].leg, 0);
        assert_eq!(samples[car_sample(&samples, 1, mid)].leg, 1);
        // Past the last leg (finished), the finish leg.
        assert_eq!(samples[car_sample(&samples, 9, mid)].leg, 1);
    }
}
