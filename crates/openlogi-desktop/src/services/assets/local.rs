//! Device art OpenLogi ships itself, for models the Logi asset index does
//! not carry (the G700 predates Options+, so no depot of it exists).
//!
//! A local depot has the layout of a synced one — `core_metadata.json`, a
//! front render (`front_core.png`) and, for a G-series mouse, a side render
//! (`side.png`), with markers keyed `<model>_g<slot>_m1` — but no
//! `index.json` entry: [`LOCAL_DEPOTS`] names the model keys it serves. It is looked up only after the index
//! found nothing, so a depot the index gains later wins.

use std::path::PathBuf;

use openlogi_assets::{DeviceEntry, FileEntry};

/// One locally shipped depot.
pub(super) struct LocalDepot {
    /// The device-list model keys it serves: a HID++ 1.0 mouse has no model
    /// info, so the gallery keys it on its receiver wpid (`wpid1023`).
    pub(super) model_keys: &'static [&'static str],
    /// Its directory under each local root.
    pub(super) depot: &'static str,
    pub(super) display_name: &'static str,
    /// The registry device type, as the asset index spells it.
    pub(super) kind: &'static str,
}

/// The depots OpenLogi ships. A depot whose files are missing resolves to
/// nothing, so the device keeps the synthetic silhouette.
pub(super) const LOCAL_DEPOTS: &[LocalDepot] = &[LocalDepot {
    model_keys: &["wpid1023"],
    depot: "g700",
    display_name: "G700",
    kind: "mouse",
}];

/// The files a local depot may hold, in the names the resolver prefers.
const LOCAL_FILES: [&str; 3] = ["core_metadata.json", "front_core.png", "side.png"];

/// The depot serving `model_key`, as an index entry would describe it.
pub(super) fn local_entry(model_key: &str) -> Option<(&'static str, DeviceEntry)> {
    let local = LOCAL_DEPOTS
        .iter()
        .find(|local| local.model_keys.contains(&model_key))?;
    Some((
        local.depot,
        DeviceEntry {
            model_id: model_key.to_owned(),
            model_ids: Vec::new(),
            display_name: local.display_name.to_owned(),
            kind: local.kind.to_owned(),
            asset_path: String::new(),
            // The resolver consults the list to tell a depot that publishes
            // hotspot metadata from one that has none; a local depot always
            // does. Hashes and sizes are for downloads, which never happen.
            files: LOCAL_FILES
                .iter()
                .map(|name| FileEntry {
                    name: (*name).to_owned(),
                    sha256: String::new(),
                    bytes: 0,
                })
                .collect(),
        },
    ))
}

/// Where local depots are read from, in order: beside the macOS bundle's
/// synced assets (`Contents/Resources/local-assets`), the Linux install's
/// share directory (`<prefix>/share/openlogi/local-assets`), and in a debug
/// build the source tree's `crates/openlogi-desktop/local-assets`. Roots
/// that do not exist are left out.
pub(super) fn local_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(prefix) = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.to_path_buf()))
    {
        roots.push(prefix.join("Resources").join("local-assets"));
        roots.push(prefix.join("share").join("openlogi").join("local-assets"));
    }
    if cfg!(debug_assertions) {
        roots.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("local-assets"));
    }
    roots.retain(|root| root.is_dir());
    roots
}
