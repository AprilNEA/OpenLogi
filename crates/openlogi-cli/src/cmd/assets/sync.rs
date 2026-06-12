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
use openlogi_assets::{FRONT_RENDER_FILES, FetchOutcome, METADATA_FILES, http};

/// Default origin. Overridable via `--base` / `OPENLOGI_ASSETS`.
const DEFAULT_BASE: &str = "https://assets.openlogi.org";

/// Returns true when `name` is an *optional* asset OpenLogi fetches when the
/// registry lists it but never warns about when it's absent:
/// - `side_core.png` / `side.png` — the dedicated buttons render, present
///   only on devices (e.g. MX Master) whose `device_buttons_image` is a
///   distinct side view. Devices that reuse the hero render don't ship one.
/// - `front_ext*.png` / `side_ext*.png` — per-colour variants for the
///   carousel and the buttons-config view. Newer depots name them
///   `front_ext_N`, older ones `front_extN`; the `front_ext` prefix covers
///   both.
///
/// (`back_*` renders stay remote until an easyswitch view needs them.)
fn is_optional_asset(name: &str) -> bool {
    if name == "side_core.png" || name == "side.png" {
        return true;
    }
    let path = std::path::Path::new(name);
    let ext_is_png = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("png"));
    if !ext_is_png {
        return false;
    }
    name.starts_with("front_ext") || name.starts_with("side_ext")
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
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let client = http::AssetClient::new(&base);
    let index = client.fetch_index_to_dir(&out)?;
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
        let dir = http::safe_component_path(&out, depot, "asset depot")?;
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        // Per-depot baseline (metadata + manifest + hero render, either
        // schema) + every optional asset (side render + colour variants)
        // the registry lists. A depot that ships no hotspot metadata or
        // hero render won't render in the GUI (cameras, receivers, bare
        // keyboards) — warn, but still bundle whatever it does have.
        let baseline = entry.baseline_files();
        let wanted: Vec<&openlogi_assets::FileEntry> = entry
            .files
            .iter()
            .filter(|f| baseline.contains(&f.name.as_str()) || is_optional_asset(&f.name))
            .collect();
        if entry.preferred_file(&METADATA_FILES).is_none() {
            eprintln!("  WARN {depot}: no hotspot metadata (core_metadata.json / metadata.json)");
        }
        if entry.preferred_file(&FRONT_RENDER_FILES).is_none() {
            eprintln!("  WARN {depot}: no hero render (front_core.png / front.png)");
        }

        for &file_entry in &wanted {
            match client.fetch_entry_if_stale(&entry.asset_path, &dir, file_entry)? {
                FetchOutcome::CacheHit => cache_hits += 1,
                FetchOutcome::Fetched { .. } => {
                    fetched += 1;
                    println!("  {depot}/{} ({} B)", file_entry.name, file_entry.bytes);
                }
            }
        }
    }

    let bundle_bytes: u64 = index
        .devices
        .values()
        .map(|d| {
            let baseline = d.baseline_files();
            d.files
                .iter()
                .filter(|f| baseline.contains(&f.name.as_str()) || is_optional_asset(&f.name))
                .map(|f| f.bytes)
                .sum::<u64>()
        })
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
