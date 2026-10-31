//! Player settings, edited in the ESC menu's Settings tab and applied live: master volume, engine
//! sound, view distance, tyre smoke and render resolution. In a browser they are remembered in the page's
//! local storage.

use bevy::prelude::*;
use serde::{Deserialize, Serialize};

use benilla_world::view::{ViewDistance, FARCLIP_RANGE};

use crate::sound::CarAudio;

/// Master volume at the start: the engine is loud next to the world.
pub const DEFAULT_VOLUME: f32 = 0.30;
/// The view distance the engine boots with (`farclip`, yards).
pub const DEFAULT_VIEW_DISTANCE: f32 = 350.0;
/// Render resolution as a share of the window, lowest first.
pub const RENDER_SCALES: [f32; 4] = [0.5, 0.67, 0.85, 1.0];

#[derive(Resource, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Master volume, 0 to 1.
    pub volume: f32,
    /// The engine's simulated sound, the costliest thing the game runs besides drawing.
    pub engine_sound: bool,
    /// View distance in yards, within [`FARCLIP_RANGE`].
    pub view_distance: f32,
    /// Puffs of tyre smoke from drifting and burnouts, on every car.
    pub tire_smoke: bool,
    /// Render resolution, one of [`RENDER_SCALES`].
    pub render_scale: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            volume: DEFAULT_VOLUME,
            engine_sound: true,
            view_distance: DEFAULT_VIEW_DISTANCE,
            tire_smoke: true,
            render_scale: 1.0,
        }
    }
}

impl Settings {
    /// What was saved, with every value pulled back into range.
    fn clamped(mut self) -> Self {
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.0, 1.0)
        } else {
            DEFAULT_VOLUME
        };
        self.view_distance = if self.view_distance.is_finite() {
            self.view_distance
                .clamp(*FARCLIP_RANGE.start(), *FARCLIP_RANGE.end())
        } else {
            DEFAULT_VIEW_DISTANCE
        };
        // The nearest offered scale.
        self.render_scale = RENDER_SCALES
            .into_iter()
            .min_by(|a, b| {
                (a - self.render_scale)
                    .abs()
                    .total_cmp(&(b - self.render_scale).abs())
            })
            .unwrap_or(1.0);
        self
    }
}

/// Which page of the ESC menu is showing.
#[derive(Resource, Clone, Copy, PartialEq, Eq, Default)]
pub enum MenuTab {
    #[default]
    FreeDrive,
    Race,
    Settings,
}

#[cfg(target_arch = "wasm32")]
mod store {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        /// The saved settings JSON, or an empty string (`web/index.html`).
        #[wasm_bindgen(js_namespace = ["globalThis", "__benillaPrefs"], js_name = load)]
        fn prefs_load() -> String;
        #[wasm_bindgen(js_namespace = ["globalThis", "__benillaPrefs"], js_name = save)]
        fn prefs_save(json: &str);
    }

    pub fn load() -> Option<String> {
        Some(prefs_load()).filter(|s| !s.is_empty())
    }

    pub fn save(json: &str) {
        prefs_save(json);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod store {
    pub fn load() -> Option<String> {
        None
    }

    pub fn save(_json: &str) {}
}

/// The render resolution is the canvas's pixel count: the page shrinks the canvas's box and the
/// browser stretches it back to the window (`web/index.html`), so winit sizes the surface to fewer
/// pixels. A native window renders at its own size.
#[cfg(target_arch = "wasm32")]
mod render_scale {
    use wasm_bindgen::prelude::*;

    #[wasm_bindgen]
    extern "C" {
        #[wasm_bindgen(js_namespace = ["globalThis", "__benillaView"], js_name = setScale)]
        fn set_scale(scale: f32);
    }

    pub fn apply(scale: f32) {
        set_scale(scale);
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod render_scale {
    pub fn apply(_scale: f32) {}
}

pub struct SettingsPlugin;

impl Plugin for SettingsPlugin {
    fn build(&self, app: &mut App) {
        let saved = store::load()
            .and_then(|s| serde_json::from_str::<Settings>(&s).ok())
            .unwrap_or_default()
            .clamped();
        app.insert_resource(saved)
            .init_resource::<MenuTab>()
            .add_systems(Update, apply_settings);
    }
}

/// Pushes the settings to the audio, the view distance and the page when they change, and remembers them.
fn apply_settings(
    settings: Res<Settings>,
    audio: Option<ResMut<CarAudio>>,
    view: Option<ResMut<ViewDistance>>,
    mut first: Local<bool>,
) {
    if !settings.is_changed() {
        return;
    }
    if let Some(mut audio) = audio {
        audio.set_master_volume(settings.volume);
        audio.set_engine_sound(settings.engine_sound);
    }
    if let Some(mut view) = view {
        view.farclip = settings.view_distance;
    }
    render_scale::apply(settings.render_scale);
    // The first run is only the loaded values arriving.
    if std::mem::replace(&mut *first, true) {
        if let Ok(json) = serde_json::to_string(&*settings) {
            store::save(&json);
        }
    }
}
