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
//!
//! The same crate builds for the browser, where the player picks their `Data` folder on the page
//! (`web/README.md`).

mod camera;
mod car;
mod engine_synth;
mod model;
#[cfg(target_arch = "wasm32")]
mod net;
mod preload;
mod settings;
mod shadow;
mod smoke;
mod sound;
mod trial;
#[cfg(target_arch = "wasm32")]
mod web;

use bevy::prelude::*;

use benilla_world::build_id::BuildId;
use benilla_world::schedule::{WorldLive, WorldStage};

fn main() -> AppExit {
    // The server-less map pick (`world_map`): 0 is Eastern Kingdoms (Redridge Mountains / Lakeshire), 1 is Kalimdor.
    // A browser has no environment to set, and the default is already 0.
    #[cfg(not(target_arch = "wasm32"))]
    if std::env::var_os("WOW_MAP").is_none() {
        std::env::set_var("WOW_MAP", "0");
    }
    // The page has handed over the player's `Data` files before it starts the engine.
    #[cfg(target_arch = "wasm32")]
    if web::on_worker() {
        return AppExit::Success;
    }
    #[cfg(target_arch = "wasm32")]
    web::mount_picked_files();
    #[cfg(target_arch = "wasm32")]
    web::start_threads();

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

    let background = benilla_world::bgwin::background_run();
    #[cfg(target_arch = "wasm32")]
    let web_window = Window {
        canvas: Some("#bevy".into()),
        fit_canvas_to_parent: true,
        prevent_default_event_handling: true,
        ..default()
    };
    #[cfg(not(target_arch = "wasm32"))]
    let web_window = Window::default();
    app.add_plugins(
        benilla_world::boot::tuned_default_plugins(Window {
            title: "benilla lambo".into(),
            resolution: (1600, 900).into(),
            focused: !background,
            ..web_window
        })
        .set(AssetPlugin {
            file_path: "assets".to_string(),
            // The page serves the car as a plain file: there is no `.meta` beside it to ask for.
            #[cfg(target_arch = "wasm32")]
            meta_check: bevy::asset::AssetMetaCheck::Never,
            ..default()
        }),
    )
    .add_plugins(benilla_world::thread_qos::ThreadQosPlugin)
    .add_plugins(benilla_world::bgwin::BgWinPlugin)
    .add_plugins(benilla_world::mac_quit::MacQuitPlugin)
    .add_plugins(benilla_world::world_plugins::WorldPlugins)
    .add_plugins(bevy::gltf::GltfPlugin::default())
    .add_plugins(bevy_egui::EguiPlugin::default());

    // Other drivers, over the page's socket to the relay (`server/`).
    #[cfg(target_arch = "wasm32")]
    app.add_plugins(net::NetPlugin);

    // Initialize audio system if sounds are present.
    if let Some(audio) = sound::CarAudio::new() {
        app.insert_resource(audio);
    }

    app.add_plugins(settings::SettingsPlugin);

    // Initialize time trial manager.
    app.init_resource::<trial::TimeTrialManager>();
    app.init_resource::<trial::VersusLobby>();
    // The picked course's tiles and models, loaded ahead of the car.
    app.init_resource::<preload::CoursePreload>();

    // The world is always live: there is no session to say otherwise.
    app.insert_resource(WorldLive(true));

    app.add_systems(
        Startup,
        (car::load_car_assets, car::spawn_car)
            .chain()
            .after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Startup,
        (
            camera::spawn_camera,
            trial::setup_markers,
            smoke::setup_tire_smoke,
            shadow::setup_car_shadow,
        )
            .after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Update,
        (
            trial::handle_trial_input,
            trial::update_versus,
            car::drive,
            camera::mouse_look,
            camera::chase,
            car::animate_wheels,
            trial::update_trial_progress,
            trial::update_course_markers,
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
            smoke::spawn_tire_smoke,
            smoke::update_tire_smoke,
            shadow::update_car_shadow,
            preload::preload_course.after(trial::handle_trial_input),
        ),
    )
    .add_systems(bevy_egui::EguiPrimaryContextPass, trial::render_trial_ui);

    // After `AssetPlugin`: the loaders go into the live `AssetServer`.
    benilla_assets::register_asset_loaders(&mut app);

    app.run()
}
