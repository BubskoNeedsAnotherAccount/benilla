//! A `Data` folder that is not on a disk: archives handed over as byte sources, the way a browser
//! build gets the files the player picked. [`mount`] records them once; [`crate::wow_data`] then
//! answers [`ROOT`], and [`crate::Chain::open`] on that path mounts them in the reference's order.
//! Nothing mounts on a native build, so its resolver is unchanged.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

pub use benilla_mpq::{Handle, Source};

/// What [`crate::wow_data`] answers while a virtual `Data` folder is mounted: a name, not a place.
pub const ROOT: &str = "benilla-virtual:/Data";

static MOUNT: OnceLock<Vec<(String, Arc<dyn Source>)>> = OnceLock::new();

/// Record the virtual `Data` folder: one source per archive file, by its file name. The first
/// call wins and a later one changes nothing.
pub fn mount(archives: Vec<(String, Arc<dyn Source>)>) {
    let _ = MOUNT.set(archives);
}

/// The mounted archives, if a virtual `Data` folder was mounted.
pub(crate) fn mounted() -> Option<&'static [(String, Arc<dyn Source>)]> {
    MOUNT.get().map(Vec::as_slice)
}

/// [`ROOT`] as a path, when a folder is mounted.
pub(crate) fn root() -> Option<PathBuf> {
    MOUNT.get().map(|_| PathBuf::from(ROOT))
}

/// Whether `path` names the mounted folder.
pub(crate) fn is_root(path: &Path) -> bool {
    path == Path::new(ROOT) && MOUNT.get().is_some()
}
