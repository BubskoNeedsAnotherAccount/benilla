//! `benilla-lambo`: drive a Lamborghini through The Barrens. A crate on top of benilla: it boots
//! the `benilla-world` engine as the world viewer does, on Kalimdor, and adds a car and a chase
//! camera. A feature 1.12.1 does not have lives here, never in benilla (`docs/CONTRIBUTING.md`,
//! "Building on top").
//!
//! ```text
//! WOW_DATA=<your 1.12.1 Data folder> cargo run --profile play -p benilla-lambo
//! ```
//!
//! Without `WOW_DATA`, a dev build looks for a `WoW` link at the repo root.

mod camera;
mod car;
mod sound;
mod trial;

use bevy::prelude::*;

use benilla_world::build_id::BuildId;
use benilla_world::schedule::{WorldLive, WorldStage};

fn main() -> AppExit {
    // The server-less map pick (`world_map`): 1 is Kalimdor, where The Barrens are.
    if std::env::var_os("WOW_MAP").is_none() {
        std::env::set_var("WOW_MAP", "1");
    }

    let mut app = App::new();
    app.insert_resource(BuildId {
        version: env!("CARGO_PKG_VERSION"),
        extended: true,
        ..Default::default()
    });

    // The `mpq://` source must be registered before `AssetPlugin` (in `DefaultPlugins`) builds.
    match benilla_formats::wow_data() {
        Some(data_dir) => {
            if let Err(e) = benilla_assets::register_mpq_source(&mut app, &data_dir) {
                eprintln!("benilla-assets: mpq:// source unavailable ({e:#})");
            }
        }
        None => eprintln!(
            "benilla-lambo: no WoW install found — looked in {:?}",
            benilla_formats::candidates()
        ),
    }

    let (asset_root, model_file, hints) = resolve_car_model();
    if let Some(ref file) = model_file {
        eprintln!("benilla-lambo: loading 3D car model: {asset_root}/{file}");
    } else {
        eprintln!(
            "benilla-lambo: no car.glb found in assets/ or crates/benilla-lambo/assets/ (using procedural car)"
        );
    }
    app.insert_resource(car::LamboModelConfig {
        model_path: model_file,
        auto_scale: hints.scale,
        auto_yaw: hints.yaw,
        auto_offset: hints.offset,
    });

    let background = benilla_world::bgwin::background_run();
    app.add_plugins(
        benilla_world::boot::tuned_default_plugins(Window {
            title: "benilla lambo".into(),
            resolution: (1600, 900).into(),
            focused: !background,
            ..default()
        })
        .set(AssetPlugin {
            file_path: asset_root,
            ..default()
        }),
    )
    .add_plugins(benilla_world::thread_qos::ThreadQosPlugin)
    .add_plugins(benilla_world::bgwin::BgWinPlugin)
    .add_plugins(benilla_world::mac_quit::MacQuitPlugin)
    .add_plugins(benilla_world::world_plugins::WorldPlugins)
    .add_plugins(bevy::gltf::GltfPlugin::default())
    .add_plugins(bevy_egui::EguiPlugin::default());

    // Initialize audio system if sounds are present.
    if let Some(audio) = sound::CarAudio::new() {
        app.insert_resource(audio);
    }

    // Initialize time trial manager.
    app.init_resource::<trial::TimeTrialManager>();

    // The world is always live: there is no session to say otherwise.
    app.insert_resource(WorldLive(true));

    app.add_systems(
        Startup,
        (car::spawn_car, camera::spawn_camera, trial::setup_markers)
            .after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Update,
        (
            trial::handle_trial_input,
            car::drive,
            camera::chase,
            car::animate_wheels,
            trial::update_trial_progress,
            trial::update_marker_positions,
            trial::animate_markers,
        )
            .chain()
            .in_set(WorldStage::Input),
    )
    .add_systems(
        Update,
        (
            camera::speedometer,
            car::prepare_car_materials,
            sound::update_car_audio,
            trial::render_trial_ui,
        ),
    );

    // After `AssetPlugin`: the loaders go into the live `AssetServer`.
    benilla_assets::register_asset_loaders(&mut app);

    app.run()
}

#[derive(Clone, Copy, Debug)]
struct GlbHints {
    scale: f32,
    yaw: f32,
    offset: Vec3,
}

impl Default for GlbHints {
    fn default() -> Self {
        Self {
            scale: 1.0,
            yaw: 0.0,
            offset: Vec3::new(0.0, -0.48, 0.0),
        }
    }
}

/// Discovers a GLB model file: checks `LAMBO_MODEL`, then common filenames in crate assets
/// and workspace assets. Returns `(asset_root, Option<relative_file_path>, GlbHints)`.
fn resolve_car_model() -> (String, Option<String>, GlbHints) {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    if let Ok(env_path) = std::env::var("LAMBO_MODEL") {
        let p = std::path::Path::new(&env_path);
        if p.exists() {
            let parent = p
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_string_lossy()
                .to_string();
            let file = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let root = if parent.is_empty() {
                ".".to_string()
            } else {
                parent
            };
            let hints = detect_glb_transform(p);
            return (root, Some(file), hints);
        } else {
            eprintln!("benilla-lambo: LAMBO_MODEL={env_path} not found on disk");
        }
    }

    let candidates = ["car.glb", "lambo.glb", "models/car.glb", "models/lambo.glb"];

    // 1. Check crate's own assets folder (crates/benilla-lambo/assets)
    for file in candidates {
        let full = manifest.join("assets").join(file);
        if full.exists() {
            let hints = detect_glb_transform(&full);
            return ("assets".to_string(), Some(file.replace('\\', "/")), hints);
        }
    }

    // 2. Check repo root assets folder (../../assets or ./assets)
    for file in candidates {
        let full_repo = manifest.join("../../assets").join(file);
        if full_repo.exists() {
            let hints = detect_glb_transform(&full_repo);
            return (
                "../../assets".to_string(),
                Some(file.replace('\\', "/")),
                hints,
            );
        }
        let full_local = std::path::Path::new("assets").join(file);
        if full_local.exists() {
            let hints = detect_glb_transform(&full_local);
            return ("assets".to_string(), Some(file.replace('\\', "/")), hints);
        }
    }

    ("assets".to_string(), None, GlbHints::default())
}

/// Inspects the GLB model geometry to determine its real dimensions, orientation, and ground contact.
/// Normalizes ANY car model to around 4.8 yards long and aligns its forward axis with -Z.
fn detect_glb_transform(full_path: &std::path::Path) -> GlbHints {
    let mut hints = GlbHints::default();

    let Ok(bytes) = std::fs::read(full_path) else {
        return hints;
    };
    if bytes.len() <= 20 || &bytes[0..4] != b"glTF" {
        return hints;
    }

    let json_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap_or_default()) as usize;
    if bytes.len() < 20 + json_len {
        return hints;
    }

    let Ok(v) = serde_json::from_slice::<serde_json::Value>(&bytes[20..20 + json_len]) else {
        return hints;
    };

    let mut min_v = [f32::INFINITY; 3];
    let mut max_v = [f32::NEG_INFINITY; 3];
    if let Some(meshes) = v.get("meshes").and_then(|m| m.as_array()) {
        for mesh in meshes {
            if let Some(primitives) = mesh.get("primitives").and_then(|p| p.as_array()) {
                for prim in primitives {
                    if let Some(pos_idx) = prim
                        .get("attributes")
                        .and_then(|a| a.get("POSITION"))
                        .and_then(|p| p.as_u64())
                    {
                        if let Some(acc) = v.get("accessors").and_then(|a| a.get(pos_idx as usize))
                        {
                            if let (Some(min_arr), Some(max_arr)) = (
                                acc.get("min").and_then(|m| m.as_array()),
                                acc.get("max").and_then(|m| m.as_array()),
                            ) {
                                if min_arr.len() == 3 && max_arr.len() == 3 {
                                    for i in 0..3 {
                                        let min_val = min_arr[i].as_f64().unwrap_or(0.0) as f32;
                                        let max_val = max_arr[i].as_f64().unwrap_or(0.0) as f32;
                                        min_v[i] = min_v[i].min(min_val);
                                        max_v[i] = max_v[i].max(max_val);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Check if root nodes apply an FBX unit matrix (e.g. 0.01 scale)
    let mut node_scale = 1.0f32;
    if let Some(nodes) = v.get("nodes").and_then(|n| n.as_array()) {
        for node in nodes {
            if let Some(matrix) = node.get("matrix").and_then(|m| m.as_array()) {
                if matrix.len() == 16 {
                    let m0 = matrix[0].as_f64().unwrap_or(1.0) as f32;
                    let m1 = matrix[1].as_f64().unwrap_or(0.0) as f32;
                    let m2 = matrix[2].as_f64().unwrap_or(0.0) as f32;
                    let sx = (m0 * m0 + m1 * m1 + m2 * m2).sqrt();
                    if sx > 0.005 && sx < 0.05 {
                        node_scale = sx;
                        break;
                    }
                }
            }
        }
    }

    if min_v[0].is_finite() && max_v[0].is_finite() {
        let extent_x = (max_v[0] - min_v[0]) * node_scale;
        let extent_y = (max_v[1] - min_v[1]) * node_scale;
        let extent_z = (max_v[2] - min_v[2]) * node_scale;

        let max_dim = extent_x.max(extent_y).max(extent_z);
        if max_dim > 0.05 {
            // Target car size is ~4.8 yards long, scaled 1.5x bigger per user request
            hints.scale = (4.8 / max_dim) * 1.5;
        }

        // If length is along X, rotate 90 deg around Y so it points forward along -Z
        if extent_x > extent_z {
            hints.yaw = 90.0;
        } else {
            hints.yaw = 0.0;
        }

        eprintln!(
            "benilla-lambo: model bounds: [{:.2}, {:.2}, {:.2}] -> auto_scale: {:.2}x, auto_yaw: {:.0}°",
            extent_x, extent_y, extent_z, hints.scale, hints.yaw
        );
    }

    hints
}
