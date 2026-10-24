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

    let (asset_root, model_file) = resolve_car_model();
    if let Some(ref file) = model_file {
        eprintln!("benilla-lambo: loading 3D car model: {asset_root}/{file}");
    } else {
        eprintln!(
            "benilla-lambo: no car.glb found in assets/ or crates/benilla-lambo/assets/ (using procedural car)"
        );
    }
    app.insert_resource(car::LamboModelConfig {
        model_path: model_file,
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
    .add_plugins(bevy::gltf::GltfPlugin::default());

    // The world is always live: there is no session to say otherwise.
    app.insert_resource(WorldLive(true));

    app.add_systems(
        Startup,
        (car::spawn_car, camera::spawn_camera).after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Update,
        (car::drive, camera::chase, car::animate_wheels)
            .chain()
            .in_set(WorldStage::Input),
    )
    .add_systems(Update, camera::speedometer);

    // After `AssetPlugin`: the loaders go into the live `AssetServer`.
    benilla_assets::register_asset_loaders(&mut app);

    app.run()
}

/// Discovers a GLB model file: checks `LAMBO_MODEL`, then common filenames in crate assets
/// and workspace assets. Returns `(asset_root, Option<relative_file_path>)`.
fn resolve_car_model() -> (String, Option<String>) {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));

    if let Ok(env_path) = std::env::var("LAMBO_MODEL") {
        let p = std::path::Path::new(&env_path);
        if p.exists() {
            let parent = p
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_string_lossy()
                .to_string();
            let file = p.file_name().unwrap_or_default().to_string_lossy().to_string();
            let root = if parent.is_empty() { ".".to_string() } else { parent };
            return (root, Some(file));
        } else {
            eprintln!("benilla-lambo: LAMBO_MODEL={env_path} not found on disk");
        }
    }

    let candidates = ["car.glb", "lambo.glb", "models/car.glb", "models/lambo.glb"];

    // 1. Check crate's own assets folder (crates/benilla-lambo/assets)
    for file in candidates {
        if manifest.join("assets").join(file).exists() {
            return ("assets".to_string(), Some(file.replace('\\', "/")));
        }
    }

    // 2. Check repo root assets folder (../../assets or ./assets)
    for file in candidates {
        if manifest.join("../../assets").join(file).exists() {
            return ("../../assets".to_string(), Some(file.replace('\\', "/")));
        }
        if std::path::Path::new("assets").join(file).exists() {
            return ("assets".to_string(), Some(file.replace('\\', "/")));
        }
    }

    ("assets".to_string(), None)
}
