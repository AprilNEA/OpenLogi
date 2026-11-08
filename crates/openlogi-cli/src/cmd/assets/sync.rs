//! `openlogi assets sync` — pull every device's bundle-required files
//! into the openlogi-desktop crate so `cargo bundle` can pick them up.
//!
//! Probes OpenLogi's asset mirrors, fetches `index.json`, writes per-device
//! files (skipping cache hits via sha256 compare) — every file
//! [`bundle_files`] selects — and prunes depots that no longer appear in the
//! registry. Default `--out` matches the cargo-bundle resources path so the
//! workflow is
//! `openlogi assets sync && cargo bundle --release`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use clap::Args;
use openlogi_assets::{
    AssetRegistry, DepotManifest, DeviceEntry, FRONT_RENDER_FILES, FetchOutcome, FileEntry,
    METADATA_FILES, http,
};

/// Returns true when `name` is an *optional* asset OpenLogi fetches when the
/// registry lists it but never warns about when it's absent:
/// - `side_core.png` / `side.png` — the dedicated buttons render, present
///   only on devices (e.g. MX Master) whose `device_buttons_image` is a
///   distinct side view. Devices that reuse the hero render don't ship one.
/// - `front_ext*.png` / `side_ext*.png` — per-colour variants for the
///   carousel and the buttons-config view. Newer depots name them
///   `front_ext_N`, older ones `front_extN`; the `front_ext` prefix covers
///   both.
/// - `core_metadata_*.json` / `metadata_*.json` — per-variant hotspot
///   metadata. Depots whose variants are *handed* rather than coloured ship
///   no bare `core_metadata.json` at all (Lift has only
///   `core_metadata_left.json` / `core_metadata_right.json`), and the depot
///   manifest's `image_metadata` is what names the right one. Bundling them
///   is what keeps such a device off the generic silhouette offline.
///
/// (`back_*` renders stay remote until an easyswitch view needs them.) These
/// are the flat names; a render the depot manifest names some other way —
/// G HUB colours (`front_white.png`), WebP, `images/device/<uuid>` — is
/// selected through the manifest instead (see [`bundle_files`]).
fn is_optional_asset(name: &str) -> bool {
    if name == "side_core.png" || name == "side.png" {
        return true;
    }
    if (name.starts_with("core_metadata_") || name.starts_with("metadata_"))
        && std::path::Path::new(name)
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
    {
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

/// The files the bundle carries for one depot: the baseline, the flat-named
/// optional assets ([`is_optional_asset`]), and every file `manifest` names
/// for a resource OpenLogi draws or reads, for every variant
/// ([`DepotManifest::consumed_srcs`]). Only names the registry lists are
/// selected, so every bundled file carries a registry hash.
fn bundle_files<'a>(
    entry: &'a DeviceEntry,
    manifest: Option<&DepotManifest>,
) -> Vec<&'a FileEntry> {
    let baseline = entry.baseline_files();
    let consumed: HashSet<&str> = manifest
        .map(DepotManifest::consumed_srcs)
        .unwrap_or_default()
        .into_iter()
        .collect();
    entry
        .files
        .iter()
        .filter(|f| {
            let name = f.name.as_str();
            baseline.contains(&name) || is_optional_asset(name) || consumed.contains(name)
        })
        .collect()
}

/// The depot manifest fetched into `dir`. `None`, with a warning, when it
/// does not parse: the depot then bundles its flat-named files only.
fn load_manifest(dir: &Path, depot: &str) -> Option<DepotManifest> {
    DepotManifest::load_from(&dir.join(MANIFEST_FILE))
        .map_err(|e| {
            eprintln!(
                "  WARN {depot}: unreadable manifest.json ({e}); bundling flat-named files only"
            );
        })
        .ok()
}

const MANIFEST_FILE: &str = "manifest.json";

/// Fetch and cache-hit counts for the closing summary.
#[derive(Debug, Default)]
struct Tally {
    fetched: u32,
    cache_hits: u32,
}

impl Tally {
    fn record(&mut self, outcome: &FetchOutcome, depot: &str, file: &FileEntry) {
        match outcome {
            FetchOutcome::CacheHit => self.cache_hits += 1,
            FetchOutcome::Fetched { .. } => {
                self.fetched += 1;
                println!("  {depot}/{} ({} B)", file.name, file.bytes);
            }
        }
    }
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Override automatic mirror discovery with one uniform asset origin.
    #[arg(long, env = openlogi_core::env::ASSETS)]
    base: Option<String>,
    /// Destination directory. Default matches the cargo-bundle
    /// resources path declared in openlogi-desktop/Cargo.toml.
    #[arg(long, default_value = "crates/openlogi-desktop/assets")]
    out: PathBuf,
}

pub fn run(args: SyncArgs) -> Result<()> {
    let SyncArgs { base, out } = args;
    fs::create_dir_all(&out).with_context(|| format!("create {}", out.display()))?;

    let registry = AssetRegistry::load(base.as_deref(), &out)?;
    let client = registry.client();
    let index = registry.index();
    println!("asset source: {}", registry.source());
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

    let mut tally = Tally::default();
    let mut bundle_bytes = 0_u64;
    let mut depots: Vec<&String> = index.devices.keys().collect();
    depots.sort();
    for depot in depots {
        let entry = &index.devices[depot];
        let dir = http::safe_component_path(&out, depot, "asset depot")?;
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

        // The manifest names the depot's variant renders, so it lands before
        // the selection that reads it.
        let manifest_file = entry.files.iter().find(|f| f.name == MANIFEST_FILE);
        if let Some(file_entry) = manifest_file {
            let outcome = client.fetch_entry_if_stale(&entry.asset_path, &dir, file_entry)?;
            tally.record(&outcome, depot, file_entry);
        }
        let manifest = manifest_file.and_then(|_| load_manifest(&dir, depot));

        // A depot that ships no hotspot metadata or hero render won't render
        // in the GUI (cameras, receivers, bare keyboards) — warn, but still
        // bundle whatever it does have.
        let wanted = bundle_files(entry, manifest.as_ref());
        if entry.preferred_file(&METADATA_FILES).is_none() {
            eprintln!("  WARN {depot}: no hotspot metadata (core_metadata.json / metadata.json)");
        }
        if entry.preferred_file(&FRONT_RENDER_FILES).is_none() {
            eprintln!("  WARN {depot}: no hero render (front_core.png / front.png)");
        }

        for &file_entry in wanted.iter().filter(|f| f.name != MANIFEST_FILE) {
            let outcome = client.fetch_entry_if_stale(&entry.asset_path, &dir, file_entry)?;
            tally.record(&outcome, depot, file_entry);
        }
        bundle_bytes += wanted.iter().map(|f| f.bytes).sum::<u64>();
    }

    #[expect(
        clippy::cast_precision_loss,
        reason = "bundle sizes are well under 2^53 bytes; f64 precision is fine for a display string"
    )]
    let mb = bundle_bytes as f64 / 1024.0 / 1024.0;
    let Tally {
        fetched,
        cache_hits,
    } = tally;
    println!(
        "done: {fetched} fetched, {cache_hits} cache-hit, {mb:.1} MB total under {}",
        out.display()
    );
    Ok(())
}

#[cfg(test)]
mod is_optional_asset_tests {
    use super::is_optional_asset;

    #[test]
    fn side_render_names_are_optional() {
        assert!(is_optional_asset("side_core.png"));
        assert!(is_optional_asset("side.png"));
    }

    #[test]
    fn colour_variant_names_are_optional() {
        assert!(is_optional_asset("front_ext1.png"));
        assert!(is_optional_asset("front_ext_2.png"));
        assert!(is_optional_asset("side_ext_3.png"));
    }

    #[test]
    fn baseline_and_metadata_files_are_not_optional() {
        assert!(!is_optional_asset("front_core.png"));
        assert!(!is_optional_asset("front.png"));
        assert!(!is_optional_asset("manifest.json"));
        assert!(!is_optional_asset("core_metadata.json"));
    }

    #[test]
    fn non_png_files_are_never_optional_even_with_a_matching_prefix() {
        assert!(!is_optional_asset("front_ext.json"));
    }

    #[test]
    fn prefix_match_is_loose_and_also_matches_unintended_names() {
        // `starts_with("front_ext")` also matches names that merely begin the
        // same way, not just the `front_extN` / `front_ext_N` schema —
        // pinning this as current behaviour, not a spec.
        assert!(is_optional_asset("front_extra_special.png"));
    }

    #[test]
    fn prefix_match_is_case_sensitive_while_the_extension_check_is_not() {
        // The extension check folds case (`eq_ignore_ascii_case`), but the
        // prefix check is a plain `starts_with`, so an all-caps name fails
        // the prefix half even though its extension would pass alone.
        assert!(!is_optional_asset("FRONT_EXT1.PNG"));
    }
}

#[cfg(test)]
mod bundle_files_tests {
    use super::bundle_files;
    use openlogi_assets::{DepotManifest, DeviceEntry, FileEntry};

    const WHITE_FRONT: &str = "images/device/e0e9dacc-b5c0-4434-8b8d-410a7aa81d2f";
    const WHITE_SIDE: &str = "images/device/22c9ac7a-43ed-48b7-8771-72ca4ae2a6bb";
    const ONBOARDING: &str = "images/device/5d1f0c4e-onboarding";

    fn depot(files: &[&str]) -> DeviceEntry {
        DeviceEntry {
            model_id: "c095".into(),
            model_ids: vec!["c095".into(), "4099".into()],
            display_name: "G502 X PLUS".into(),
            kind: "MOUSE".into(),
            asset_path: "v1/devices/g502x_plus/".into(),
            files: files
                .iter()
                .map(|name| FileEntry {
                    name: (*name).into(),
                    sha256: String::new(),
                    bytes: 1,
                })
                .collect(),
        }
    }

    fn names<'a>(files: &[&'a FileEntry]) -> Vec<&'a str> {
        files.iter().map(|f| f.name.as_str()).collect()
    }

    /// #2459: the G502 X PLUS keeps only `front.png` at its depot root and
    /// names every colour and side render by path; the flat-name selection
    /// bundled none of them.
    #[test]
    fn a_g_hub_depot_bundles_the_renders_its_manifest_names() {
        let entry = depot(&[
            "metadata.json",
            "manifest.json",
            "front.png",
            WHITE_FRONT,
            WHITE_SIDE,
            "front_white.png",
            ONBOARDING,
        ]);
        let manifest: DepotManifest = serde_json::from_str(&format!(
            r#"{{"devices":[
                {{"modelId":"g502x_plus","resources":[
                    {{"key":"device_image","src":"front.png"}},
                    {{"key":"onboarding_data","src":"{ONBOARDING}"}}]}},
                {{"modelId":"g502x_plus_ext1","resources":[
                    {{"key":"device_image","src":"{WHITE_FRONT}"}},
                    {{"key":"device_side","src":"{WHITE_SIDE}"}}]}},
                {{"modelId":"g502x_plus_ext2","resources":[
                    {{"key":"device_image","src":"front_white.png"}}]}}
            ]}}"#
        ))
        .expect("manifest parses");

        assert_eq!(
            names(&bundle_files(&entry, Some(&manifest))),
            [
                "metadata.json",
                "manifest.json",
                "front.png",
                WHITE_FRONT,
                WHITE_SIDE,
                "front_white.png",
            ]
        );
    }

    #[test]
    fn a_manifest_src_the_registry_does_not_list_is_not_bundled() {
        let entry = depot(&["metadata.json", "manifest.json", "front.png"]);
        let manifest: DepotManifest = serde_json::from_str(&format!(
            r#"{{"devices":[{{"modelId":"g502x_plus_ext1","resources":[
                {{"key":"device_image","src":"{WHITE_FRONT}"}}]}}]}}"#
        ))
        .expect("manifest parses");

        assert_eq!(
            names(&bundle_files(&entry, Some(&manifest))),
            ["metadata.json", "manifest.json", "front.png"]
        );
    }

    #[test]
    fn without_a_manifest_only_the_flat_names_are_bundled() {
        let entry = depot(&[
            "core_metadata.json",
            "manifest.json",
            "front_core.png",
            "side_core.png",
            "front_ext_1.png",
            "back_core.png",
            WHITE_FRONT,
        ]);

        assert_eq!(
            names(&bundle_files(&entry, None)),
            [
                "core_metadata.json",
                "manifest.json",
                "front_core.png",
                "side_core.png",
                "front_ext_1.png",
            ]
        );
    }
}
