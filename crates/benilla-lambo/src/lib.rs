//! `benilla-lambo` library and web runtime.
//! Provides car physics, time trial system, chase camera, procedural audio, and WebAssembly entry point.

pub mod camera;
pub mod car;
pub mod engine_synth;
pub mod shadow;
pub mod smoke;
pub mod sound;
pub mod trial;

use std::sync::Arc;
use bevy::prelude::*;

use benilla_world::build_id::BuildId;
use benilla_world::schedule::{WorldLive, WorldStage};

#[derive(Clone, Copy, Debug)]
pub struct GlbHints {
    pub scale: f32,
    pub yaw: f32,
    pub offset: Vec3,
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

/// Sets up the core Bevy systems, resources, and plugins for `benilla-lambo`.
pub fn configure_lambo_app(app: &mut App, asset_root: String, model_file: Option<String>, hints: GlbHints) {
    app.insert_resource(BuildId {
        version: env!("CARGO_PKG_VERSION"),
        extended: true,
        ..Default::default()
    });

    app.insert_resource(car::LamboModelConfig {
        model_path: model_file,
        auto_scale: hints.scale,
        auto_yaw: hints.yaw,
        auto_offset: hints.offset,
    });

    #[cfg(not(target_arch = "wasm32"))]
    let background = benilla_world::bgwin::background_run();
    #[cfg(target_arch = "wasm32")]
    let background = false;

    let primary_window = Window {
        title: "benilla lambo".into(),
        resolution: (1600, 900).into(),
        focused: !background,
        canvas: Some("#bevy".into()),
        fit_canvas_to_parent: true,
        ..default()
    };

    app.add_plugins(
        benilla_world::boot::tuned_default_plugins(primary_window)
            .set(AssetPlugin {
                file_path: asset_root,
                ..default()
            }),
    );

    #[cfg(not(target_arch = "wasm32"))]
    {
        app.add_plugins(benilla_world::thread_qos::ThreadQosPlugin)
            .add_plugins(benilla_world::bgwin::BgWinPlugin)
            .add_plugins(benilla_world::mac_quit::MacQuitPlugin);
    }

    app.add_plugins(benilla_world::world_plugins::WorldPlugins)
        .add_plugins(bevy::gltf::GltfPlugin::default())
        .add_plugins(bevy_egui::EguiPlugin::default());

    if let Some(audio) = sound::CarAudio::new() {
        app.insert_resource(audio);
    }

    app.init_resource::<trial::TimeTrialManager>();
    app.insert_resource(WorldLive(true));

    app.add_systems(
        Startup,
        (
            car::spawn_car,
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
            smoke::spawn_tire_smoke,
            smoke::update_tire_smoke,
            shadow::update_car_shadow,
        ),
    )
    .add_systems(bevy_egui::EguiPrimaryContextPass, trial::render_trial_ui);

    benilla_assets::register_asset_loaders(app);
}

// ---------------------------------------------------------------------------
// DESKTOP RUNNER
// ---------------------------------------------------------------------------
#[cfg(not(target_arch = "wasm32"))]
pub fn run_desktop() -> AppExit {
    if std::env::var_os("WOW_MAP").is_none() {
        std::env::set_var("WOW_MAP", "0");
    }

    let mut app = App::new();

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

    let (asset_root, model_file, hints) = resolve_car_model_desktop();
    if let Some(ref file) = model_file {
        eprintln!("benilla-lambo: loading 3D car model: {asset_root}/{file}");
    } else {
        eprintln!(
            "benilla-lambo: no car.glb found in assets/ or crates/benilla-lambo/assets/ (using procedural car)"
        );
    }

    configure_lambo_app(&mut app, asset_root, model_file, hints);
    app.run()
}

#[cfg(not(target_arch = "wasm32"))]
fn resolve_car_model_desktop() -> (String, Option<String>, GlbHints) {
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates = ["car.glb", "lambo.glb", "models/car.glb", "models/lambo.glb"];

    for file in candidates {
        let full = manifest.join("assets").join(file);
        if full.exists() {
            let hints = detect_glb_transform(&full);
            return ("assets".to_string(), Some(file.replace('\\', "/")), hints);
        }
    }

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

#[cfg(not(target_arch = "wasm32"))]
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
            hints.scale = (4.8 / max_dim) * 1.5;
        }

        if extent_x > extent_z {
            hints.yaw = 90.0;
        } else {
            hints.yaw = 0.0;
        }
    }

    hints
}

// ---------------------------------------------------------------------------
// WEBASSEMBLY (WASM) RUNNER
// ---------------------------------------------------------------------------
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_namespace = console)]
    fn log(s: &str);

    #[wasm_bindgen(catch)]
    fn js_read_mpq_slice_sync(archive_name: &str, offset: f64, length: f64) -> Result<js_sys::Uint8Array, JsValue>;

    fn js_get_mpq_size(archive_name: &str) -> f64;
}

#[cfg(target_arch = "wasm32")]
pub struct WebMpqStream {
    name: String,
    pos: u64,
    size: u64,
}

#[cfg(target_arch = "wasm32")]
impl WebMpqStream {
    pub fn new(name: String) -> Self {
        let size = js_get_mpq_size(&name);
        Self {
            name,
            pos: 0,
            size: if size >= 0.0 { size as u64 } else { 0 },
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl std::io::Read for WebMpqStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let to_read = (buf.len() as u64).min(self.size - self.pos);
        match js_read_mpq_slice_sync(&self.name, self.pos as f64, to_read as f64) {
            Ok(uint8_arr) => {
                let len = uint8_arr.length() as usize;
                uint8_arr.copy_to(&mut buf[..len]);
                self.pos += len as u64;
                Ok(len)
            }
            Err(e) => Err(std::io::Error::other(format!("{e:?}"))),
        }
    }
}

#[cfg(target_arch = "wasm32")]
impl std::io::Seek for WebMpqStream {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let new_pos = match pos {
            std::io::SeekFrom::Start(offset) => offset as i64,
            std::io::SeekFrom::Current(offset) => self.pos as i64 + offset,
            std::io::SeekFrom::End(offset) => self.size as i64 + offset,
        };
        if new_pos < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid seek position",
            ));
        }
        self.pos = new_pos as u64;
        Ok(self.pos)
    }
}

#[cfg(target_arch = "wasm32")]
impl benilla_mpq::MpqStream for WebMpqStream {
    fn stream_len(&mut self) -> std::io::Result<u64> {
        Ok(self.size)
    }
}

/// WebAssembly entry point called by JavaScript after the user selects their WoW `Data` folder.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn start_game_wasm(archive_names: js_sys::Array) -> Result<(), JsValue> {
    console_error_panic_hook::set_once();

    let mut names = Vec::new();
    for i in 0..archive_names.length() {
        if let Some(s) = archive_names.get(i).as_string() {
            names.push(s);
        }
    }

    let mount_list = benilla_formats::mount_order(&names);
    let mut archives = Vec::new();
    for name in mount_list {
        let archive_name = name.clone();
        let opener: benilla_mpq::StreamOpener = Arc::new(move || {
            Ok(Box::new(WebMpqStream::new(archive_name.clone())) as Box<dyn benilla_mpq::MpqStream>)
        });
        match benilla_mpq::Archive::open_from_stream(&name, opener) {
            Ok(arc) => archives.push(arc),
            Err(e) => log(&format!("benilla-lambo: warning opening {name}: {e}")),
        }
    }

    if archives.is_empty() {
        return Err(JsValue::from_str("No valid WoW MPQ archives could be opened"));
    }

    let chain = Arc::new(benilla_formats::Chain::from_archives(archives));
    let mut app = App::new();
    benilla_assets::register_mpq_source_with_chain(&mut app, chain);

    configure_lambo_app(&mut app, "assets".to_string(), None, GlbHints::default());
    app.run();
    Ok(())
}
