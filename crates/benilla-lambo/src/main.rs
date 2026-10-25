//! `benilla-lambo` binary entry point.

fn main() {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = benilla_lambo::run_desktop();
    }
}
