//! `benilla-lambo` binary entry point.

#[cfg(not(target_arch = "wasm32"))]
fn main() {
    let _ = benilla_lambo::run_desktop();
}

#[cfg(target_arch = "wasm32")]
fn main() {}
