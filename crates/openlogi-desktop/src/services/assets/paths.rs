//! Asset filesystem roots and index loading.

use std::path::PathBuf;

use openlogi_assets::{INDEX_NAME, Index};
use tracing::{debug, warn};

/// Per-user writable cache root: `openlogi_core::paths::data_dir()` plus an
/// `assets/` subdir, keeping the render cache out of the config dir. Falls
/// back to `./assets` only when no home directory can be resolved.
pub(super) fn user_cache_root() -> PathBuf {
    openlogi_core::paths::data_dir()
        .map_or_else(|_| PathBuf::from("./assets"), |d| d.join("assets"))
}

/// Read-only root pointing inside the macOS `.app` bundle when the binary
/// is launched from one: `<exe_dir>/../Resources/assets/`. The probe also
/// requires an `index.json` inside — an empty dir (e.g. `cargo bundle`
/// run without first syncing) is treated as not-present so the runtime
/// HTTP fallback can still recover.
pub(super) fn bundle_assets_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.parent()?.join("Resources").join("assets");
    candidate.join(INDEX_NAME).is_file().then_some(candidate)
}

/// Walk read roots looking for the first parseable `index.json`. Bundle
/// wins over user cache so a release-time snapshot stays authoritative.
pub(super) fn load_index(roots: &[PathBuf]) -> Option<Index> {
    for root in roots {
        let path = root.join(INDEX_NAME);
        if !path.exists() {
            continue;
        }
        match Index::load_from(&path) {
            Ok(idx) => {
                debug!(
                    devices = idx.devices.len(),
                    root = %root.display(),
                    "asset index loaded"
                );
                return Some(idx);
            }
            Err(e) => {
                warn!(error = ?e, root = %root.display(), "failed to parse asset index");
            }
        }
    }
    debug!("no asset index found — using synthetic silhouette for all devices");
    None
}
