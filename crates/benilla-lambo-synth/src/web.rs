//! The synth's browser face: `web/synth-worker.js` makes one of these and renders blocks into the
//! shared ring buffer the game reads (`benilla-lambo/web/synth-bridge.js`).

use wasm_bindgen::prelude::*;

use crate::{Params, Synth};

#[wasm_bindgen]
pub struct WebSynth {
    synth: Synth,
}

#[wasm_bindgen]
impl WebSynth {
    #[wasm_bindgen(constructor)]
    pub fn new(engine_id: &str, muffler: &str, quality: &str, burble: f32, sample_rate: u32) -> WebSynth {
        console_error_panic_hook::set_once();
        WebSynth {
            synth: Synth::new(engine_id, muffler, quality, burble, sample_rate),
        }
    }

    pub fn set_sample_rate(&mut self, rate: u32) {
        self.synth.set_sample_rate(rate);
    }

    /// Fill `out` with samples for the given state.
    pub fn render(&mut self, rpm: f32, throttle: f32, volume: f32, speed: f32, shifting: bool, out: &mut [f32]) {
        self.synth.render(
            &Params {
                rpm,
                throttle,
                volume,
                speed,
                shifting,
            },
            out,
        );
    }
}
