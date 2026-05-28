//! `openlogi assets sync` — pull every device's bundle-required files
//! into the openlogi-gui crate so `cargo bundle` can pick them up.
//!
//! Fetches `index.json`, writes per-device files (skipping cache hits via
//! sha256 compare), and prunes depots that no longer appear in the
//! registry. Default `--out` matches the cargo-bundle resources path so
//! the workflow is `openlogi assets sync && cargo bundle --release`.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context as _, Result};
use clap::Args;
use openlogi_assets::http;

/// Default origin. Overridable via `--base` / `OPENLOGI_ASSETS`.
const DEFAULT_BASE: &str = "https://assets.openlogi.org";

/// Required per-depot files. `AssetCache` always reads
/// `core_metadata.json` for hotspot layout; `manifest.json` drives the
/// HID++ `extended_model_id` → colour variant lookup; `front_core.png`
/// is the fallback render when the device's variant isn't cached.
const REQUIRED_FILES: &[&str] = &["core_metadata.json", "manifest.json", "front_core.png"];

/// Returns true when `name` is a front-view colour variant PNG
/// (`front_ext_1.png`, `front_ext_2.png`, …). The depot also ships
/// `side_*` and `back_*` renders — those stay remote until a future
/// scroll / easyswitch view needs them.
fn is_variant_front(name: &str) -> bool {
    name.starts_with("front_ext_")
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("png"))
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Origin of the asset host.
    #[arg(long, default_value = DEFAULT_BASE, env = "OPENLOGI_ASSETS")]
    base: String,
    /// Destination directory. Default matches the cargo-bundle
    /// resources path declared in openlogi-gui/Cargo.toml.
    #[arg(long, default_value = "crates/openlogi-gui/assets")]
    out: PathBuf,
}

pub fn run(args: SyncArgs) -> Result<()> {
    let SyncArgs { base, out } = args;
    let base = base.trim_end_matches('/').to_string();
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let index = http::fetch_index_to_dir(&base, &out)?;
    println!("index.json: {} devices", index.devices.len());

    // Prune orphans so the bundle stays in sync with the registry.
    let expected: HashSet<&str> = index.devices.keys().map(String::as_str).collect();
    for entry in fs::read_dir(&out)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if !expected.contains(name_str.as_ref()) {
            println!("  pruning {name_str}");
            fs::remove_dir_all(entry.path())?;
        }
    }

    let mut fetched = 0_u32;
    let mut cache_hits = 0_u32;
    let mut depots: Vec<&String> = index.devices.keys().collect();
    depots.sort();
    for depot in depots {
        let entry = &index.devices[depot];
        let dir = out.join(depot);
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        // Required core set + every front-view colour variant the
        // registry knows about. The depot manifest is fetched
        // unconditionally so `AssetCache` can route HID++ ext bytes to
        // the right PNG; missing files trip a per-file warning.
        let wanted: Vec<&openlogi_assets::FileEntry> = entry
            .files
            .iter()
            .filter(|f| REQUIRED_FILES.contains(&f.name.as_str()) || is_variant_front(&f.name))
            .collect();
        for required in REQUIRED_FILES {
            if !wanted.iter().any(|f| f.name == *required) {
                eprintln!("  WARN {depot}: registry missing {required}");
            }
        }

        for file_entry in &wanted {
            let dst = dir.join(&file_entry.name);
            if http::cached_matches(&dst, &file_entry.sha256) {
                cache_hits += 1;
                continue;
            }
            http::fetch_file_to_dir(&base, &entry.asset_path, &dir, &file_entry.name)?;
            fetched += 1;
            println!("  {depot}/{} ({} B)", file_entry.name, file_entry.bytes);
        }
    }

    let bundle_bytes: u64 = index
        .devices
        .values()
        .flat_map(|d| d.files.iter())
        .filter(|f| REQUIRED_FILES.contains(&f.name.as_str()) || is_variant_front(&f.name))
        .map(|f| f.bytes)
        .sum();
    #[allow(
        clippy::cast_precision_loss,
        reason = "bundle sizes are well under 2^53 bytes; f64 precision is fine for a display string"
    )]
    let mb = bundle_bytes as f64 / 1024.0 / 1024.0;
    println!(
        "done: {fetched} fetched, {cache_hits} cache-hit, {mb:.1} MB total under {}",
        out.display()
    );
    Ok(())
}
